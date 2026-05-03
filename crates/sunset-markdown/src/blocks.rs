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

        if let Some(rest) = lines[i].strip_prefix(">>> ") {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
            // Take this line plus all remaining lines, joined back.
            let mut quoted = vec![rest];
            quoted.extend_from_slice(&lines[i + 1..]);
            let inner = quoted.join("\n");
            blocks.push(Block::Quote(split(&inner)));
            return blocks;
        }

        if let Some(rest) = lines[i].strip_prefix("> ") {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
            // Group consecutive `> ` lines into one quote block.
            let mut quoted = vec![rest.to_owned()];
            let mut j = i + 1;
            while j < lines.len() {
                if let Some(more) = lines[j].strip_prefix("> ") {
                    quoted.push(more.to_owned());
                    j += 1;
                } else {
                    break;
                }
            }
            let inner = quoted.join("\n");
            blocks.push(Block::Quote(split(&inner)));
            i = j;
            continue;
        }

        if lines[i].starts_with("- ") {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
            let mut items: Vec<Vec<Block>> = Vec::new();
            let mut j = i;
            while j < lines.len() {
                if let Some(rest) = lines[j].strip_prefix("- ") {
                    items.push(split(rest));
                    j += 1;
                } else {
                    break;
                }
            }
            blocks.push(Block::UnorderedList(items));
            i = j;
            continue;
        }

        if let Some((level, rest)) = match_heading(lines[i]) {
            if !buf.is_empty() {
                blocks.push(paragraph_from_lines(&buf));
                buf.clear();
            }
            blocks.push(Block::Heading {
                level,
                content: crate::inline::parse_inlines(rest),
            });
            i += 1;
            continue;
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

fn match_heading(line: &str) -> Option<(crate::HeadingLevel, &str)> {
    use crate::HeadingLevel;
    if let Some(rest) = line.strip_prefix("### ") {
        return Some((HeadingLevel::H3, rest));
    }
    if let Some(rest) = line.strip_prefix("## ") {
        return Some((HeadingLevel::H2, rest));
    }
    if let Some(rest) = line.strip_prefix("# ") {
        return Some((HeadingLevel::H1, rest));
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
