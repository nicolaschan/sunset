//! Discord-flavored markdown parser used by all sunset.chat clients.
//!
//! See `docs/superpowers/specs/2026-05-02-message-markdown-design.md`.

#![cfg_attr(not(feature = "serde"), allow(dead_code))]

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document(pub Vec<Block>);

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: HeadingLevel,
        content: Vec<Inline>,
    },
    Quote(Vec<Block>),
    UnorderedList(Vec<Vec<Block>>),
    CodeBlock {
        language: Option<String>,
        source: String,
    },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Underline(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Spoiler(Vec<Inline>),
    InlineCode(String),
    Link {
        label: Vec<Inline>,
        url: String,
        autolink: bool,
    },
    LineBreak,
}

mod blocks;
mod inline;

/// Parse a message body into a `Document`. Total: malformed input
/// degrades to literal text rather than erroring.
pub fn parse(input: &str) -> Document {
    Document(blocks::split(input))
}

/// Render a `Document` back to a flat string with all formatting markers
/// stripped. Idempotent on already-plain text.
pub fn to_plain(_doc: &Document) -> String {
    // Stub. Replaced by Task A16.
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_empty_document() {
        assert_eq!(parse(""), Document(Vec::new()));
    }

    #[test]
    fn plain_text_is_one_paragraph() {
        assert_eq!(
            parse("hello"),
            Document(vec![Block::Paragraph(vec![Inline::Text("hello".to_owned())])])
        );
    }

    #[test]
    fn blank_line_splits_paragraphs() {
        assert_eq!(
            parse("first\n\nsecond"),
            Document(vec![
                Block::Paragraph(vec![Inline::Text("first".to_owned())]),
                Block::Paragraph(vec![Inline::Text("second".to_owned())]),
            ])
        );
    }

    #[test]
    fn single_newline_in_paragraph_is_line_break() {
        assert_eq!(
            parse("first\nsecond"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("first".to_owned()),
                Inline::LineBreak,
                Inline::Text("second".to_owned()),
            ])])
        );
    }

    #[test]
    fn trailing_blank_lines_dont_emit_empty_paragraph() {
        assert_eq!(
            parse("hello\n\n\n"),
            Document(vec![Block::Paragraph(vec![Inline::Text("hello".to_owned())])])
        );
    }

    #[test]
    fn bold_wraps_inner_text() {
        assert_eq!(
            parse("a **b** c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Bold(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn unclosed_bold_degrades_to_literal_text() {
        assert_eq!(
            parse("a **b c"),
            Document(vec![Block::Paragraph(vec![Inline::Text("a **b c".to_owned())])])
        );
    }

    #[test]
    fn empty_bold_pair_collapses_to_literal_text() {
        assert_eq!(
            parse("a **** b"),
            Document(vec![Block::Paragraph(vec![Inline::Text("a **** b".to_owned())])])
        );
    }

    #[test]
    fn italic_with_asterisk() {
        assert_eq!(
            parse("a *b* c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Italic(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn italic_with_underscore() {
        assert_eq!(
            parse("a _b_ c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Italic(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn underscore_inside_word_is_literal() {
        // Discord behavior: `foo_bar` does NOT italicize — `_` only opens
        // italic at a word boundary. We approximate with "preceding char
        // is not alphanumeric".
        assert_eq!(
            parse("foo_bar_baz"),
            Document(vec![Block::Paragraph(vec![Inline::Text(
                "foo_bar_baz".to_owned()
            )])])
        );
    }

    #[test]
    fn bold_wraps_italic() {
        assert_eq!(
            parse("**a *b* c**"),
            Document(vec![Block::Paragraph(vec![Inline::Bold(vec![
                Inline::Text("a ".to_owned()),
                Inline::Italic(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])])
        );
    }

    #[test]
    fn underline_double_underscore() {
        assert_eq!(
            parse("a __b__ c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Underline(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn underline_wraps_italic() {
        assert_eq!(
            parse("__a _b_ c__"),
            Document(vec![Block::Paragraph(vec![Inline::Underline(vec![
                Inline::Text("a ".to_owned()),
                Inline::Italic(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])])
        );
    }

    #[test]
    fn strikethrough() {
        assert_eq!(
            parse("a ~~b~~ c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Strikethrough(vec![Inline::Text("b".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn single_tilde_is_literal() {
        assert_eq!(
            parse("a ~b~ c"),
            Document(vec![Block::Paragraph(vec![Inline::Text(
                "a ~b~ c".to_owned()
            )])])
        );
    }

    #[test]
    fn spoiler() {
        assert_eq!(
            parse("a ||secret|| c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::Spoiler(vec![Inline::Text("secret".to_owned())]),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn spoiler_wraps_bold() {
        assert_eq!(
            parse("||spoiler with **bold**||"),
            Document(vec![Block::Paragraph(vec![Inline::Spoiler(vec![
                Inline::Text("spoiler with ".to_owned()),
                Inline::Bold(vec![Inline::Text("bold".to_owned())]),
            ])])])
        );
    }

    #[test]
    fn inline_code() {
        assert_eq!(
            parse("a `b` c"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("a ".to_owned()),
                Inline::InlineCode("b".to_owned()),
                Inline::Text(" c".to_owned()),
            ])])
        );
    }

    #[test]
    fn inline_code_does_not_parse_markdown_inside() {
        assert_eq!(
            parse("`**not bold**`"),
            Document(vec![Block::Paragraph(vec![Inline::InlineCode(
                "**not bold**".to_owned()
            )])])
        );
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        assert_eq!(
            parse("a `b c"),
            Document(vec![Block::Paragraph(vec![Inline::Text("a `b c".to_owned())])])
        );
    }

    #[test]
    fn empty_backtick_pair_is_literal() {
        assert_eq!(
            parse("``"),
            Document(vec![Block::Paragraph(vec![Inline::Text("``".to_owned())])])
        );
    }

    #[test]
    fn masked_link_basic() {
        assert_eq!(
            parse("see [the docs](https://example.com) here"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("see ".to_owned()),
                Inline::Link {
                    label: vec![Inline::Text("the docs".to_owned())],
                    url: "https://example.com".to_owned(),
                    autolink: false,
                },
                Inline::Text(" here".to_owned()),
            ])])
        );
    }

    #[test]
    fn masked_link_label_can_contain_bold() {
        assert_eq!(
            parse("[**important**](https://x.com)"),
            Document(vec![Block::Paragraph(vec![Inline::Link {
                label: vec![Inline::Bold(vec![Inline::Text("important".to_owned())])],
                url: "https://x.com".to_owned(),
                autolink: false,
            }])])
        );
    }

    #[test]
    fn unbalanced_brackets_are_literal() {
        // The `[docs(` part has no matching `](`, so it stays as literal text.
        // The bare `https://` URL inside it still autolinks; `)` is trailing
        // punctuation and gets excluded from the URL.
        assert_eq!(
            parse("see [docs(https://example.com) here"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("see [docs(".to_owned()),
                Inline::Link {
                    label: vec![Inline::Text("https://example.com".to_owned())],
                    url: "https://example.com".to_owned(),
                    autolink: true,
                },
                Inline::Text(") here".to_owned()),
            ])])
        );
    }

    #[test]
    fn autolink_https() {
        assert_eq!(
            parse("see https://example.com here"),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("see ".to_owned()),
                Inline::Link {
                    label: vec![Inline::Text("https://example.com".to_owned())],
                    url: "https://example.com".to_owned(),
                    autolink: true,
                },
                Inline::Text(" here".to_owned()),
            ])])
        );
    }

    #[test]
    fn autolink_excludes_trailing_punctuation() {
        assert_eq!(
            parse("visit https://example.com."),
            Document(vec![Block::Paragraph(vec![
                Inline::Text("visit ".to_owned()),
                Inline::Link {
                    label: vec![Inline::Text("https://example.com".to_owned())],
                    url: "https://example.com".to_owned(),
                    autolink: true,
                },
                Inline::Text(".".to_owned()),
            ])])
        );
    }

    #[test]
    fn autolink_at_start() {
        assert_eq!(
            parse("https://example.com"),
            Document(vec![Block::Paragraph(vec![Inline::Link {
                label: vec![Inline::Text("https://example.com".to_owned())],
                url: "https://example.com".to_owned(),
                autolink: true,
            }])])
        );
    }

    #[test]
    fn fenced_code_block_no_language() {
        assert_eq!(
            parse("```\nhello world\n```"),
            Document(vec![Block::CodeBlock {
                language: None,
                source: "hello world".to_owned(),
            }])
        );
    }

    #[test]
    fn fenced_code_block_with_language() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```"),
            Document(vec![Block::CodeBlock {
                language: Some("rust".to_owned()),
                source: "fn main() {}".to_owned(),
            }])
        );
    }

    #[test]
    fn fenced_code_block_preserves_internal_blank_lines() {
        assert_eq!(
            parse("```\nline 1\n\nline 3\n```"),
            Document(vec![Block::CodeBlock {
                language: None,
                source: "line 1\n\nline 3".to_owned(),
            }])
        );
    }

    #[test]
    fn fenced_code_block_does_not_parse_markdown_inside() {
        assert_eq!(
            parse("```\n**not bold**\n```"),
            Document(vec![Block::CodeBlock {
                language: None,
                source: "**not bold**".to_owned(),
            }])
        );
    }

    #[test]
    fn unclosed_fenced_code_falls_back_to_paragraph() {
        // Implementation choice: we treat the whole input as paragraphs
        // when the closing fence is missing, rather than swallowing
        // everything into an unterminated code block. This matches Discord.
        let result = parse("```\nhello");
        // The leading ``` line has no closer, so the block splitter
        // emits it as a paragraph containing "```" then "hello".
        assert_eq!(
            result,
            Document(vec![Block::Paragraph(vec![
                Inline::Text("```".to_owned()),
                Inline::LineBreak,
                Inline::Text("hello".to_owned()),
            ])])
        );
    }
}
