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

/// Parse a message body into a `Document`. Total: malformed input
/// degrades to literal text rather than erroring.
pub fn parse(input: &str) -> Document {
    // Stub. Replaced incrementally by Tasks A2..A15.
    if input.is_empty() {
        Document(Vec::new())
    } else {
        Document(vec![Block::Paragraph(vec![Inline::Text(input.to_owned())])])
    }
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
}
