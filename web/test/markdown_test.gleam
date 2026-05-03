//// Tests exercise `markdown.render_blocks` (the pure renderer) with
//// hand-built AST values, NOT the full `markdown.render` path that
//// goes through FFI. Going through FFI here would require WASM to be
//// loaded in the `gleam test` Node environment, which it isn't.
//// The full pipeline (body → WASM → AST → render) is covered by the
//// Playwright e2e tests in `web/e2e/`.

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
