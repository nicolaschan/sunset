//// Tests exercise `markdown.render_blocks` (the pure renderer) with
//// hand-built AST values, NOT the full `markdown.render` path that
//// goes through FFI. Going through FFI here would require WASM to be
//// loaded in the `gleam test` Node environment, which it isn't.
//// The full pipeline (body → WASM → AST → render) is covered by the
//// Playwright e2e tests in `web/e2e/`.

import gleam/option
import gleam/string
import gleeunit/should
import lustre/element
import sunset_web/markdown
import sunset_web/theme

fn p() {
  theme.palette_for(theme.Dark)
}

fn render_html(blocks) {
  markdown.render_blocks(blocks, "msg-1", fn(_) { False }, fn(_) { Nil }, p())
  |> element.to_string()
}

pub fn render_plain_text_test() {
  let html = render_html([markdown.Paragraph([markdown.Text("hello")])])
  should.be_true(string.contains(html, "<p"))
  should.be_true(string.contains(html, "hello"))
}

pub fn render_bold_test() {
  let html =
    render_html([
      markdown.Paragraph([
        markdown.Text("a "),
        markdown.Bold([markdown.Text("b")]),
        markdown.Text(" c"),
      ]),
    ])
  should.be_true(string.contains(html, "<strong>b</strong>"))
  should.be_true(string.contains(html, "a "))
  should.be_true(string.contains(html, " c"))
}

pub fn render_italic_test() {
  let html =
    render_html([
      markdown.Paragraph([markdown.Italic([markdown.Text("b")])]),
    ])
  should.be_true(string.contains(html, "<em>b</em>"))
}

pub fn render_underline_test() {
  let html =
    render_html([
      markdown.Paragraph([markdown.Underline([markdown.Text("b")])]),
    ])
  should.be_true(string.contains(html, "<u>b</u>"))
}

pub fn render_strikethrough_test() {
  let html =
    render_html([
      markdown.Paragraph([markdown.Strikethrough([markdown.Text("b")])]),
    ])
  should.be_true(string.contains(html, "<s>b</s>"))
}

pub fn render_inline_code_test() {
  let html =
    render_html([markdown.Paragraph([markdown.InlineCode("b")])])
  should.be_true(string.contains(html, "<code"))
  should.be_true(string.contains(html, ">b</code>"))
}

pub fn render_line_break_test() {
  let html =
    render_html([
      markdown.Paragraph([
        markdown.Text("a"),
        markdown.LineBreak,
        markdown.Text("b"),
      ]),
    ])
  should.be_true(string.contains(html, "<br"))
  should.be_true(string.contains(html, "a"))
  should.be_true(string.contains(html, "b"))
}

pub fn render_spoiler_hidden_test() {
  let html =
    markdown.render_blocks(
      [markdown.Paragraph([markdown.Spoiler([markdown.Text("secret")])])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "color: transparent"))
  should.be_true(string.contains(html, "secret"))
}

pub fn render_spoiler_revealed_test() {
  let html =
    markdown.render_blocks(
      [markdown.Paragraph([markdown.Spoiler([markdown.Text("secret")])])],
      "msg-1",
      fn(_) { True },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_false(string.contains(html, "color: transparent"))
  should.be_true(string.contains(html, "secret"))
}

pub fn render_masked_link_test() {
  let html =
    markdown.render_blocks(
      [markdown.Paragraph([markdown.Link([markdown.Text("click")], "https://example.com", False)])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<a "))
  should.be_true(string.contains(html, "href=\"https://example.com\""))
  should.be_true(string.contains(html, "target=\"_blank\""))
  should.be_true(string.contains(html, "rel=\"noopener noreferrer\""))
  should.be_true(string.contains(html, "title=\"https://example.com\""))
  should.be_true(string.contains(html, ">click</a>"))
}

pub fn render_autolink_omits_title_test() {
  let url = "https://example.com"
  let html =
    markdown.render_blocks(
      [markdown.Paragraph([markdown.Link([markdown.Text(url)], url, True)])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<a "))
  should.be_true(string.contains(html, "href=\"https://example.com\""))
  should.be_false(string.contains(html, "title="))
}

pub fn render_disallowed_scheme_renders_as_text_test() {
  let html =
    markdown.render_blocks(
      [markdown.Paragraph([markdown.Link([markdown.Text("bad")], "javascript:alert(1)", False)])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  // No <a> tag, but the URL must still be visible somewhere.
  should.be_false(string.contains(html, "<a "))
  should.be_true(string.contains(html, "javascript:"))
}

pub fn render_heading_test() {
  let html =
    markdown.render_blocks(
      [markdown.Heading(1, [markdown.Text("title")])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<h1"))
  should.be_true(string.contains(html, "title"))
}

pub fn render_quote_test() {
  let html =
    markdown.render_blocks(
      [markdown.Quote([markdown.Paragraph([markdown.Text("hello")])])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<blockquote"))
  should.be_true(string.contains(html, "hello"))
}

pub fn render_unordered_list_test() {
  let html =
    markdown.render_blocks(
      [markdown.UnorderedList([
        [markdown.Paragraph([markdown.Text("one")])],
        [markdown.Paragraph([markdown.Text("two")])],
      ])],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<ul"))
  should.be_true(string.contains(html, "<li"))
  should.be_true(string.contains(html, "one"))
  should.be_true(string.contains(html, "two"))
}

pub fn render_code_block_with_language_test() {
  let html =
    markdown.render_blocks(
      [markdown.CodeBlock(option.Some("rust"), "fn main() {}")],
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      p(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<pre"))
  should.be_true(string.contains(html, "<code"))
  should.be_true(string.contains(html, "rust"))
  should.be_true(string.contains(html, "fn main()"))
}

// to_plain goes through FFI and needs WASM loaded. In the `gleam test`
// Node environment (no WASM bundle), the FFI falls back to returning
// the body unchanged. In production (WASM loaded), formatting markers
// are stripped and only the plain text remains.
// Either way, the words must be present in the output.
pub fn to_plain_returns_something_with_text_test() {
  let result = markdown.to_plain("hello **bold**")
  // Test env (no WASM): returns "hello **bold**" (body verbatim).
  // Production (WASM loaded): returns "hello bold" (markers stripped).
  should.be_true(string.contains(result, "hello"))
  should.be_true(string.contains(result, "bold"))
}
