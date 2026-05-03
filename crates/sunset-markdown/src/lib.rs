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
}
