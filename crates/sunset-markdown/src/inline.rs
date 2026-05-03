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
        // Inline code: scan to next backtick, no markdown inside.
        // Reject empty pair `` so it stays literal.
        if bytes[i] == b'`' {
            if let Some(end) = find_byte(bytes, b'`', i + 1) {
                if end > i + 1 {
                    if text_start < i {
                        out.push(Inline::Text(input[text_start..i].to_owned()));
                    }
                    out.push(Inline::InlineCode(input[i + 1..end].to_owned()));
                    i = end + 1;
                    text_start = i;
                    continue;
                }
            }
        }

        if bytes[i] == b'[' {
            if let Some((label_end, url_end)) = find_link_parts(bytes, i) {
                if text_start < i {
                    out.push(Inline::Text(input[text_start..i].to_owned()));
                }
                let label = parse_inlines(&input[i + 1..label_end]);
                let url = input[label_end + 2..url_end].to_owned();
                out.push(Inline::Link {
                    label,
                    url,
                    autolink: false,
                });
                i = url_end + 1;
                text_start = i;
                continue;
            }
        }

        if let Some(url_end) = match_autolink(bytes, i) {
            if text_start < i {
                out.push(Inline::Text(input[text_start..i].to_owned()));
            }
            let url = &input[i..url_end];
            out.push(Inline::Link {
                label: vec![Inline::Text(url.to_owned())],
                url: url.to_owned(),
                autolink: true,
            });
            i = url_end;
            text_start = i;
            continue;
        }

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

/// If `bytes[i..]` starts with `http://` or `https://`, returns the
/// exclusive end index of the URL (after stripping trailing punctuation).
fn match_autolink(bytes: &[u8], i: usize) -> Option<usize> {
    let starts = bytes.get(i..i + 7) == Some(b"http://")
        || bytes.get(i..i + 8) == Some(b"https://");
    if !starts {
        return None;
    }
    let mut j = i;
    while j < bytes.len() && is_url_byte(bytes[j]) {
        j += 1;
    }
    while j > i && is_trailing_punct(bytes[j - 1]) {
        j -= 1;
    }
    if j == i { None } else { Some(j) }
}

fn is_url_byte(b: u8) -> bool {
    !b.is_ascii_whitespace() && !matches!(b, b'<' | b'>' | b'"' | b'`' | b'|')
}

fn is_trailing_punct(b: u8) -> bool {
    matches!(b, b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'\'' | b'"')
}

fn find_byte(bytes: &[u8], target: u8, from: usize) -> Option<usize> {
    bytes[from..].iter().position(|&b| b == target).map(|p| p + from)
}

/// Returns `(label_end, url_end)` where `bytes[i] = '['`,
/// `bytes[label_end] = ']'`, `bytes[label_end+1] = '('`, and
/// `bytes[url_end] = ')'`. Returns None if not a well-formed link.
fn find_link_parts(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let label_end = find_byte(bytes, b']', i + 1)?;
    if bytes.get(label_end + 1) != Some(&b'(') {
        return None;
    }
    let url_end = find_byte(bytes, b')', label_end + 2)?;
    Some((label_end, url_end))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    Bold,          // **
    Underline,     // __
    Strikethrough, // ~~
    Spoiler,       // ||
    ItalicStar,    // *
    ItalicUnder,   // _
}

fn match_delimiter(bytes: &[u8], i: usize) -> Option<(Delim, usize)> {
    // Order matters: longer markers first so `**` wins over `*` and `__` wins over `_`.
    if bytes.get(i..i + 2) == Some(b"**") {
        // Reject empty pair `****` (would otherwise produce Bold(empty)).
        if bytes.get(i + 2..i + 4) == Some(b"**") {
            return None;
        }
        return Some((Delim::Bold, 2));
    }
    if bytes.get(i..i + 2) == Some(b"__") {
        if bytes.get(i + 2..i + 4) == Some(b"__") {
            return None;
        }
        return Some((Delim::Underline, 2));
    }
    if bytes.get(i..i + 2) == Some(b"~~") {
        if bytes.get(i + 2..i + 4) == Some(b"~~") {
            return None;
        }
        return Some((Delim::Strikethrough, 2));
    }
    if bytes.get(i..i + 2) == Some(b"||") {
        if bytes.get(i + 2..i + 4) == Some(b"||") {
            return None;
        }
        return Some((Delim::Spoiler, 2));
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
        Delim::Underline => Inline::Underline(inner),
        Delim::Strikethrough => Inline::Strikethrough(inner),
        Delim::Spoiler => Inline::Spoiler(inner),
        Delim::ItalicStar | Delim::ItalicUnder => Inline::Italic(inner),
    }
}
