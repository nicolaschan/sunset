//! Inline parser. Parses a single block's worth of text into a
//! sequence of `Inline` nodes. The block splitter calls this for each
//! Paragraph (and later Heading/Quote/ListItem).
//!
//! Strategy: byte-by-byte scan. When we encounter a known opening
//! delimiter, look forward for its closing match. If found, recurse
//! on the inner span; otherwise treat the delimiter as literal text.
//!
//! Discord's grammar lets the same character sequence (e.g. `**`)
//! act as both opener and closer, so each delimiter has a fixed
//! pair length and we use a greedy "first matching close" rule.

use crate::Inline;

pub(crate) fn parse_inlines(input: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut text_start = 0;

    while i < bytes.len() {
        if let Some((kind, marker_len)) = match_delimiter(bytes, i) {
            if let Some(close) = find_close(bytes, i + marker_len, kind) {
                // Flush pending plain text.
                if text_start < i {
                    out.push(Inline::Text(input[text_start..i].to_owned()));
                }
                let inner = &input[i + marker_len..close];
                out.push(wrap(kind, parse_inlines(inner)));
                i = close + marker_len;
                text_start = i;
                continue;
            }
        }
        i += 1;
    }
    if text_start < bytes.len() {
        out.push(Inline::Text(input[text_start..].to_owned()));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    Bold,        // **
    ItalicStar,  // *
    ItalicUnder, // _
}

fn match_delimiter(bytes: &[u8], i: usize) -> Option<(Delim, usize)> {
    // Order matters: longer markers first so `**` wins over `*`.
    if bytes.get(i..i + 2) == Some(b"**") {
        // Reject empty pair `****` (would otherwise produce Bold(empty)).
        if bytes.get(i + 2..i + 4) == Some(b"**") {
            return None;
        }
        return Some((Delim::Bold, 2));
    }
    if bytes.get(i) == Some(&b'*') {
        return Some((Delim::ItalicStar, 1));
    }
    if bytes.get(i) == Some(&b'_') && is_word_boundary(bytes, i) {
        return Some((Delim::ItalicUnder, 1));
    }
    None
}

fn is_word_boundary(bytes: &[u8], i: usize) -> bool {
    // `_` only opens/closes italic if the previous (for opener) or
    // next (for closer) character is not an ASCII alphanumeric.
    // We use the same predicate for both because we re-enter
    // match_delimiter at each scan position.
    let prev = if i == 0 { None } else { bytes.get(i - 1).copied() };
    let next = bytes.get(i + 1).copied();
    !is_word_byte(prev) || !is_word_byte(next)
}

fn is_word_byte(b: Option<u8>) -> bool {
    matches!(b, Some(c) if c.is_ascii_alphanumeric())
}

fn find_close(bytes: &[u8], start: usize, kind: Delim) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() {
        if let Some((k, len)) = match_delimiter(bytes, j) {
            if k == kind && j > start {
                // Closer must not be immediately adjacent to opener
                // (rejects `****`-style empty pairs at scan time too).
                return Some(j);
            }
            j += len;
        } else {
            j += 1;
        }
    }
    None
}

fn wrap(kind: Delim, inner: Vec<Inline>) -> Inline {
    match kind {
        Delim::Bold => Inline::Bold(inner),
        Delim::ItalicStar | Delim::ItalicUnder => Inline::Italic(inner),
    }
}
