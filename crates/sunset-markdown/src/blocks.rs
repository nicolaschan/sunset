//! Block-level splitter: turns the raw input into block boundaries
//! (Paragraph, future Heading/Quote/etc.) Inline parsing is delegated.

use crate::{Block, Inline};

/// Split `input` into blocks. Non-blank runs become Paragraphs; fenced
/// code blocks (``` … ```) become CodeBlock.
pub(crate) fn split(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = input.split('\n').collect();
    let mut i = 0;
    let mut buf: Vec<&str> = Vec::new();

    while i < lines.len() {
        if let Some(lang_part) = fence_open(lines[i]) {
            // Look for closing fence.
            if let Some(close_idx) = find_fence_close(&lines, i + 1) {
                if !buf.is_empty() {
                    blocks.push(paragraph_from_lines(&buf));
                    buf.clear();
                }
                let language = if lang_part.is_empty() {
                    None
                } else {
                    Some(lang_part.to_owned())
                };
                let source = lines[i + 1..close_idx].join("\n");
                blocks.push(Block::CodeBlock { language, source });
                i = close_idx + 1;
                continue;
            }
        }

        if lines[i].is_empty() {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
        } else {
            buf.push(lines[i]);
        }
        i += 1;
    }
    if !buf.is_empty() {
        blocks.push(paragraph_from_lines(&buf));
    }
    blocks
}

/// If `line` is exactly ` ``` ` followed by an optional language tag
/// (no trailing content other than whitespace), returns the language
/// (possibly empty). Otherwise returns None.
fn fence_open(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let rest = line.strip_prefix("```")?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

fn find_fence_close(lines: &[&str], from: usize) -> Option<usize> {
    for (offset, line) in lines[from..].iter().enumerate() {
        if line.trim() == "```" {
            return Some(from + offset);
        }
    }
    None
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
