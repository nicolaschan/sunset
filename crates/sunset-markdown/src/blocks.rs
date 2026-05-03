//! Block-level splitter: turns the raw input into block boundaries
//! (Paragraph, future Heading/Quote/etc.) Inline parsing is delegated.

use crate::{Block, Inline};

/// Split `input` into blocks. For Phase A2 every non-blank run becomes
/// a single Paragraph; subsequent tasks add Heading/Quote/List/CodeBlock.
pub(crate) fn split(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut buf: Vec<&str> = Vec::new();

    for line in input.split('\n') {
        if line.is_empty() {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
        } else {
            buf.push(line);
        }
    }
    if !buf.is_empty() {
        blocks.push(paragraph_from_lines(&buf));
    }
    blocks
}

fn paragraph_from_lines(lines: &[&str]) -> Block {
    use crate::inline::parse_inlines;
    let mut content: Vec<Inline> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            content.push(Inline::LineBreak);
        }
        content.extend(parse_inlines(line));
    }
    Block::Paragraph(content)
}
