# Message Markdown Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Discord-flavored markdown rendering for chat message bodies, with a Cmd+B/I/K-aware textarea composer that supports newlines.

**Architecture:** A new pure-Rust `sunset-markdown` crate parses bodies into a typed AST. `sunset-web-wasm` exposes `parse_markdown` to JS via `serde-wasm-bindgen`. The Gleam web app calls it through a new FFI shim and renders the AST to Lustre nodes. The wire format is unchanged: bodies stay raw markdown strings.

**Tech Stack:** Rust (parser), `wasm-bindgen` + `serde-wasm-bindgen` (bridge), Gleam + Lustre (renderer), Playwright (e2e).

**Spec:** [`docs/superpowers/specs/2026-05-02-message-markdown-design.md`](../specs/2026-05-02-message-markdown-design.md)

**Branch / worktree:** `feature/markdown` in `.worktrees/markdown/`.

**Conventions used by this plan:**
- Run cargo commands via `nix develop --command cargo …` (per `CLAUDE.md`).
- Run Gleam tests via `nix develop --command gleam test` from the `web/` directory.
- Run Playwright via `nix develop --command npx playwright test` from `web/`.
- Each task ends in a commit with the trailer `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

---

## Phase A — `sunset-markdown` parser

The crate is pure Rust, no I/O, no rendering, no `wasm-bindgen`. It must build for both `wasm32-unknown-unknown` and the host target. Workspace lints (`unsafe_code = deny`) apply.

### Task A1: Create `sunset-markdown` crate skeleton with AST types

**Files:**
- Create: `crates/sunset-markdown/Cargo.toml`
- Create: `crates/sunset-markdown/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create `crates/sunset-markdown/Cargo.toml`**

```toml
[package]
name = "sunset-markdown"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[lints]
workspace = true

[dependencies]
serde = { workspace = true, optional = true }

[features]
default = []
serde = ["dep:serde", "serde/derive"]
```

- [ ] **Step 2: Add `sunset-markdown` to workspace members**

In the root `Cargo.toml`, append `"crates/sunset-markdown"` to the `[workspace] members` array. Keep alphabetical-ish order; insert before `"crates/sunset-noise"`.

Also add to `[workspace.dependencies]` (alphabetical with the other `sunset-*` lines):

```toml
sunset-markdown = { path = "crates/sunset-markdown", version = "0.1.0" }
```

If `serde` isn't already in `[workspace.dependencies]`, add it:

```toml
serde = { version = "1", default-features = false, features = ["alloc", "derive"] }
```

(Verify whether `serde` is already there before adding.)

- [ ] **Step 3: Write the AST types and a stub `parse` in `src/lib.rs`**

```rust
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
```

- [ ] **Step 4: Verify it builds and tests pass**

Run from the repo root:

```
nix develop --command cargo build -p sunset-markdown
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 2 tests passing.

- [ ] **Step 5: Commit**

```
git add Cargo.toml crates/sunset-markdown
git commit -m "sunset-markdown: crate skeleton with AST and stub parser

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A2: Block splitter — paragraphs, blank lines, soft line breaks

A blank line ends a block. A single newline within a paragraph emits an `Inline::LineBreak` (rendered later as `<br>`).

**Files:**
- Modify: `crates/sunset-markdown/src/lib.rs`
- Create: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests for paragraph splitting and soft breaks**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 failures (`blank_line_splits_paragraphs`, `single_newline_in_paragraph_is_line_break`, `trailing_blank_lines_dont_emit_empty_paragraph`).

- [ ] **Step 3: Create `crates/sunset-markdown/src/blocks.rs`**

```rust
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
    let mut content: Vec<Inline> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            content.push(Inline::LineBreak);
        }
        // Phase A2: each line is a single Text node. Tasks A3..A11
        // replace this with the inline parser.
        content.push(Inline::Text((*line).to_owned()));
    }
    Block::Paragraph(content)
}
```

- [ ] **Step 4: Wire it into `parse` in `src/lib.rs`**

Replace the existing `pub fn parse(input: &str) -> Document { … }` with:

```rust
mod blocks;

pub fn parse(input: &str) -> Document {
    Document(blocks::split(input))
}
```

(Move the `mod blocks;` near the top of the file with any other `mod` lines, or just below the doc comment.)

Delete the now-redundant `if input.is_empty() { … } else { … }` body — it's gone in the rewrite above.

- [ ] **Step 5: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 5 tests passing.

- [ ] **Step 6: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: block splitter with paragraphs and soft line breaks

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A3: Inline parser scaffolding + bold (`**`)

This task introduces the inline parser as a separate module and implements its first feature, bold. Subsequent tasks plug new delimiters into the same scanner.

**Files:**
- Create: `crates/sunset-markdown/src/inline.rs`
- Modify: `crates/sunset-markdown/src/lib.rs`
- Modify: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests for bold parsing**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 new failures.

- [ ] **Step 3: Create `crates/sunset-markdown/src/inline.rs`**

```rust
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
    Bold, // **
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
    None
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
    }
}
```

- [ ] **Step 4: Wire `parse_inlines` into the block splitter**

In `src/blocks.rs`, replace the existing `paragraph_from_lines` body with:

```rust
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
```

- [ ] **Step 5: Add `mod inline;` to `src/lib.rs`**

Add `mod inline;` next to the existing `mod blocks;`.

- [ ] **Step 6: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 8 tests passing (the previous 5 + 3 new). The `single_newline_in_paragraph_is_line_break` test still passes because each line is now run through `parse_inlines`, which leaves bare text alone.

- [ ] **Step 7: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: inline parser scaffolding + bold

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A4: Italic (`*…*` and `_…_`)

Italic is a one-character delimiter (`*` or `_`). It must not collide with bold (`**`) or underline (`__`) — those are matched first because they're longer.

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests for italic**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 4 new failures.

- [ ] **Step 3: Extend the delimiter set**

In `src/inline.rs`, change `Delim` to include italic, and update `match_delimiter` and `wrap`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    Bold,        // **
    ItalicStar,  // *
    ItalicUnder, // _
}

fn match_delimiter(bytes: &[u8], i: usize) -> Option<(Delim, usize)> {
    if bytes.get(i..i + 2) == Some(b"**") {
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

fn wrap(kind: Delim, inner: Vec<Inline>) -> Inline {
    match kind {
        Delim::Bold => Inline::Bold(inner),
        Delim::ItalicStar | Delim::ItalicUnder => Inline::Italic(inner),
    }
}
```

- [ ] **Step 4: Adjust `find_close` so it pairs same-kind closers**

The existing `find_close` already requires `k == kind`. Confirm it still does. With three Delim variants now, `Bold` pairs with `Bold`, `ItalicStar` pairs with `ItalicStar`, `ItalicUnder` pairs with `ItalicUnder`. No code change needed.

- [ ] **Step 5: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 12 tests passing.

- [ ] **Step 6: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: italic with * and _ (word-boundary aware)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A5: Underline (`__…__`)

`__` is a two-character delimiter that must be matched before single `_` (italic). Discord renders `__x__` as underline, *not* bold.

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 2 new failures.

- [ ] **Step 3: Extend `Delim` and `match_delimiter`**

In `src/inline.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    Bold,        // **
    Underline,   // __
    ItalicStar,  // *
    ItalicUnder, // _
}

fn match_delimiter(bytes: &[u8], i: usize) -> Option<(Delim, usize)> {
    if bytes.get(i..i + 2) == Some(b"**") {
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
    if bytes.get(i) == Some(&b'*') {
        return Some((Delim::ItalicStar, 1));
    }
    if bytes.get(i) == Some(&b'_') && is_word_boundary(bytes, i) {
        return Some((Delim::ItalicUnder, 1));
    }
    None
}

fn wrap(kind: Delim, inner: Vec<Inline>) -> Inline {
    match kind {
        Delim::Bold => Inline::Bold(inner),
        Delim::Underline => Inline::Underline(inner),
        Delim::ItalicStar | Delim::ItalicUnder => Inline::Italic(inner),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 14 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: underline (__) takes precedence over italic _

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A6: Strikethrough (`~~…~~`)

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 2 new failures.

- [ ] **Step 3: Add `Strikethrough` to `Delim`**

In `src/inline.rs`:

Add `Strikethrough,` to the `Delim` enum.

In `match_delimiter`, add (positioned before any single-char delimiters):

```rust
    if bytes.get(i..i + 2) == Some(b"~~") {
        if bytes.get(i + 2..i + 4) == Some(b"~~") {
            return None;
        }
        return Some((Delim::Strikethrough, 2));
    }
```

In `wrap`:

```rust
        Delim::Strikethrough => Inline::Strikethrough(inner),
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 16 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: strikethrough (~~)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A7: Spoiler (`||…||`)

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 2 new failures.

- [ ] **Step 3: Add `Spoiler` to `Delim`**

In `src/inline.rs`:

Add `Spoiler,` to the `Delim` enum.

In `match_delimiter`:

```rust
    if bytes.get(i..i + 2) == Some(b"||") {
        if bytes.get(i + 2..i + 4) == Some(b"||") {
            return None;
        }
        return Some((Delim::Spoiler, 2));
    }
```

In `wrap`:

```rust
        Delim::Spoiler => Inline::Spoiler(inner),
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 18 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: spoiler (||)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A8: Inline code (`` ` ``)

Inline code is special: no markdown is parsed inside, and the contents become a single `String` (not `Vec<Inline>`).

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 4 new failures.

- [ ] **Step 3: Handle inline code as a special case in `parse_inlines`**

In `src/inline.rs`, modify `parse_inlines` so it special-cases backtick BEFORE checking other delimiters. The `end > i + 1` guard rejects empty-pair `` `` `` so it falls through to literal text.

```rust
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

        if let Some((kind, marker_len)) = match_delimiter(bytes, i) {
            if let Some(close) = find_close(bytes, i + marker_len, kind) {
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

fn find_byte(bytes: &[u8], target: u8, from: usize) -> Option<usize> {
    bytes[from..].iter().position(|&b| b == target).map(|p| p + from)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: all tests pass. (The exact count after this task is 22, but the count drifts as you add more tasks — the important thing is that everything green.)

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: inline code (no markdown parsed inside)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A9: Masked links `[label](url)`

`[label]` parses inline content. `(url)` is raw — anything until matching `)`. If either part is malformed, the whole thing is literal text.

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
    assert_eq!(
        parse("see [docs(https://example.com) here"),
        Document(vec![Block::Paragraph(vec![Inline::Text(
            "see [docs(https://example.com) here".to_owned()
        )])])
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 new failures.

- [ ] **Step 3: Handle masked links in `parse_inlines`**

In `src/inline.rs`, just below the inline-code special case in `parse_inlines`, add:

```rust
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
```

And below `find_byte`, add `find_link_parts`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 24 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: masked links [label](url)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A10: Autolinks (`http(s)://…`)

Bare `http://` and `https://` URLs become `Inline::Link { autolink: true }`. The URL ends at the first whitespace, `>`, `)`, or end of input. Trailing punctuation (`.,;:!?`) is excluded from the URL.

**Files:**
- Modify: `crates/sunset-markdown/src/inline.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 new failures.

- [ ] **Step 3: Detect autolinks in `parse_inlines`**

In `src/inline.rs`, just below the masked-link block in `parse_inlines`, add:

```rust
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
```

Add the `match_autolink` helper near `find_link_parts`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 27 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: autolinks for http(s)://

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A11: Block — fenced code blocks

A fenced code block starts with a line whose first non-whitespace content is exactly ` ``` ` (optionally followed by a language), and ends at the next line that is exactly ` ``` `. No markdown is parsed inside.

**Files:**
- Modify: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 5 new failures.

- [ ] **Step 3: Rewrite the block splitter to recognize fences**

Replace `split` in `src/blocks.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 32 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: fenced code blocks with optional language

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A12: Block — block quotes (`>` and `>>>`)

`> text` quotes a single line. `>>> ` quotes everything until end of message. Leading `>`/`>>>` is stripped before recursing into block parsing on the inner content.

**Files:**
- Modify: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
#[test]
fn single_line_quote() {
    assert_eq!(
        parse("> hello"),
        Document(vec![Block::Quote(vec![Block::Paragraph(vec![
            Inline::Text("hello".to_owned())
        ])])])
    );
}

#[test]
fn quote_only_on_consecutive_lines() {
    assert_eq!(
        parse("> hello\nworld"),
        Document(vec![
            Block::Quote(vec![Block::Paragraph(vec![Inline::Text("hello".to_owned())])]),
            Block::Paragraph(vec![Inline::Text("world".to_owned())]),
        ])
    );
}

#[test]
fn block_quote_to_end() {
    assert_eq!(
        parse(">>> hello\nworld\nmore"),
        Document(vec![Block::Quote(vec![Block::Paragraph(vec![
            Inline::Text("hello".to_owned()),
            Inline::LineBreak,
            Inline::Text("world".to_owned()),
            Inline::LineBreak,
            Inline::Text("more".to_owned()),
        ])])])
    );
}

#[test]
fn quote_can_contain_bold() {
    assert_eq!(
        parse("> **important**"),
        Document(vec![Block::Quote(vec![Block::Paragraph(vec![
            Inline::Bold(vec![Inline::Text("important".to_owned())])
        ])])])
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 4 new failures.

- [ ] **Step 3: Add quote handling to `split` in `src/blocks.rs`**

Inside the `while i < lines.len()` loop in `split`, after the existing fence check, add (before the blank-line check):

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 36 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: block quotes (> and >>>)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A13: Block — unordered lists (`- item`)

A run of consecutive lines starting with `- ` becomes one `UnorderedList`. Each item's content is parsed as a `Vec<Block>` (so an item can hold a paragraph; future tasks could add nested lists, but they're out of scope here).

**Files:**
- Modify: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
#[test]
fn unordered_list() {
    assert_eq!(
        parse("- one\n- two\n- three"),
        Document(vec![Block::UnorderedList(vec![
            vec![Block::Paragraph(vec![Inline::Text("one".to_owned())])],
            vec![Block::Paragraph(vec![Inline::Text("two".to_owned())])],
            vec![Block::Paragraph(vec![Inline::Text("three".to_owned())])],
        ])])
    );
}

#[test]
fn list_item_can_contain_inline_formatting() {
    assert_eq!(
        parse("- **bold** item"),
        Document(vec![Block::UnorderedList(vec![vec![Block::Paragraph(vec![
            Inline::Bold(vec![Inline::Text("bold".to_owned())]),
            Inline::Text(" item".to_owned()),
        ])])])])
    );
}

#[test]
fn list_ends_at_non_list_line() {
    assert_eq!(
        parse("- one\n- two\nafter"),
        Document(vec![
            Block::UnorderedList(vec![
                vec![Block::Paragraph(vec![Inline::Text("one".to_owned())])],
                vec![Block::Paragraph(vec![Inline::Text("two".to_owned())])],
            ]),
            Block::Paragraph(vec![Inline::Text("after".to_owned())]),
        ])
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 new failures.

- [ ] **Step 3: Add list handling to `split` in `src/blocks.rs`**

Inside the `while i < lines.len()` loop in `split`, after the quote checks (and before the blank-line check), add:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 39 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: unordered lists

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A14: Block — headings (`# `, `## `, `### `)

A line starting with `# ` (h1), `## ` (h2), or `### ` (h3) is a heading. The remainder of the line is parsed as inline content.

**Files:**
- Modify: `crates/sunset-markdown/src/blocks.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
#[test]
fn h1_h2_h3() {
    assert_eq!(
        parse("# one\n## two\n### three"),
        Document(vec![
            Block::Heading {
                level: HeadingLevel::H1,
                content: vec![Inline::Text("one".to_owned())],
            },
            Block::Heading {
                level: HeadingLevel::H2,
                content: vec![Inline::Text("two".to_owned())],
            },
            Block::Heading {
                level: HeadingLevel::H3,
                content: vec![Inline::Text("three".to_owned())],
            },
        ])
    );
}

#[test]
fn h4_or_more_is_paragraph() {
    assert_eq!(
        parse("#### not a heading"),
        Document(vec![Block::Paragraph(vec![Inline::Text(
            "#### not a heading".to_owned()
        )])])
    );
}

#[test]
fn heading_can_contain_inline_formatting() {
    assert_eq!(
        parse("# **bold** title"),
        Document(vec![Block::Heading {
            level: HeadingLevel::H1,
            content: vec![
                Inline::Bold(vec![Inline::Text("bold".to_owned())]),
                Inline::Text(" title".to_owned()),
            ],
        }])
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 3 new failures.

- [ ] **Step 3: Add heading handling to `split` in `src/blocks.rs`**

Inside the `while i < lines.len()` loop in `split`, after the list check (and before the blank-line check), add:

```rust
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
```

Add helper at the bottom of `blocks.rs`:

```rust
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
```

(Order matters: longest prefix first.)

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 42 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: ATX headings (# ## ###)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A15: `to_plain` implementation

Strips all formatting and returns concatenated text. Used by callers that need a notification body or `aria-label`.

**Files:**
- Modify: `crates/sunset-markdown/src/lib.rs`

- [ ] **Step 1: Write failing tests**

In `src/lib.rs` `mod tests`, append:

```rust
#[test]
fn to_plain_strips_inline_formatting() {
    let doc = parse("hello **bold** _italic_ `code`");
    assert_eq!(to_plain(&doc), "hello bold italic code");
}

#[test]
fn to_plain_renders_paragraphs_with_blank_line_separator() {
    let doc = parse("first\n\nsecond");
    assert_eq!(to_plain(&doc), "first\n\nsecond");
}

#[test]
fn to_plain_renders_link_label() {
    let doc = parse("[click](https://x.com)");
    assert_eq!(to_plain(&doc), "click");
}

#[test]
fn to_plain_renders_autolink_url() {
    let doc = parse("see https://x.com");
    assert_eq!(to_plain(&doc), "see https://x.com");
}

#[test]
fn to_plain_renders_code_block() {
    let doc = parse("```\nfn main() {}\n```");
    assert_eq!(to_plain(&doc), "fn main() {}");
}

#[test]
fn to_plain_length_does_not_exceed_input() {
    // Every character in `to_plain(parse(s))` is also in `s`. We can't
    // assert byte equality, but we can assert the output isn't longer.
    for input in &["hi", "**bold**", "[a](https://x.com)", "# title"] {
        assert!(to_plain(&parse(input)).len() <= input.len(), "input: {input:?}");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 6 new failures.

- [ ] **Step 3: Implement `to_plain`**

Replace the stub `to_plain` in `src/lib.rs` with:

```rust
pub fn to_plain(doc: &Document) -> String {
    let mut out = String::new();
    for (i, block) in doc.0.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        write_block(&mut out, block);
    }
    out
}

fn write_block(out: &mut String, block: &Block) {
    match block {
        Block::Paragraph(inlines) | Block::Heading { content: inlines, .. } => {
            for il in inlines {
                write_inline(out, il);
            }
        }
        Block::Quote(blocks) => {
            for (i, b) in blocks.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n\n");
                }
                write_block(out, b);
            }
        }
        Block::UnorderedList(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                for (j, b) in item.iter().enumerate() {
                    if j > 0 {
                        out.push_str("\n\n");
                    }
                    write_block(out, b);
                }
            }
        }
        Block::CodeBlock { source, .. } => {
            out.push_str(source);
        }
    }
}

fn write_inline(out: &mut String, il: &Inline) {
    match il {
        Inline::Text(s) | Inline::InlineCode(s) => out.push_str(s),
        Inline::Bold(xs)
        | Inline::Italic(xs)
        | Inline::Underline(xs)
        | Inline::Strikethrough(xs)
        | Inline::Spoiler(xs) => {
            for x in xs {
                write_inline(out, x);
            }
        }
        Inline::Link { label, .. } => {
            for x in label {
                write_inline(out, x);
            }
        }
        Inline::LineBreak => out.push('\n'),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 48 tests passing.

- [ ] **Step 5: Commit**

```
git add crates/sunset-markdown
git commit -m "sunset-markdown: to_plain strips formatting

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A16: Property tests — `parse` is total

Add `proptest` so we can fuzz `parse` against random inputs without crashes.

**Files:**
- Modify: `crates/sunset-markdown/Cargo.toml`
- Modify: `Cargo.toml` (workspace deps)
- Create: `crates/sunset-markdown/tests/property.rs`

- [ ] **Step 1: Add `proptest` to workspace dev-deps**

In the root `Cargo.toml` `[workspace.dependencies]`, add (if not already present):

```toml
proptest = "1"
```

- [ ] **Step 2: Add `proptest` as a dev-dep in `crates/sunset-markdown/Cargo.toml`**

Append:

```toml
[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 3: Write the property test file**

Create `crates/sunset-markdown/tests/property.rs`:

```rust
//! `parse` must be total: never panic on any UTF-8 input.

use proptest::prelude::*;
use sunset_markdown::{parse, to_plain};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn parse_does_not_panic(input in "\\PC*") {
        let _ = parse(&input);
    }

    #[test]
    fn parse_then_plain_does_not_grow(input in "\\PC*") {
        let doc = parse(&input);
        let plain = to_plain(&doc);
        prop_assert!(plain.len() <= input.len());
    }
}
```

- [ ] **Step 4: Run the property tests**

```
nix develop --command cargo nextest run -p sunset-markdown
```

Expected: 50 tests passing (48 unit + 2 property; nextest treats each `proptest!` block as one test).

- [ ] **Step 5: Commit**

```
git add Cargo.toml crates/sunset-markdown
git commit -m "sunset-markdown: property tests for total parse

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task A17: Workspace lint check

Run the project-standard lint and format checks on the new crate to make sure it complies with workspace rules.

- [ ] **Step 1: Run clippy and fmt check**

```
nix develop --command cargo clippy -p sunset-markdown --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
```

Both must succeed with no output. If clippy flags anything, fix it (idiomatic refactors only; no behavior change). If fmt complains, run `cargo fmt --all` and re-verify.

- [ ] **Step 2: Commit any cleanup**

If you made any changes, commit them with a small message like `sunset-markdown: clippy and fmt cleanup`.

---

## Phase B — WASM bridge

### Task B1: Add `parse_markdown` export to `sunset-web-wasm` with wasm-bindgen-test

**Files:**
- Modify: `crates/sunset-web-wasm/Cargo.toml`
- Create: `crates/sunset-web-wasm/src/markdown.rs`
- Modify: `crates/sunset-web-wasm/src/lib.rs`
- Create: `crates/sunset-web-wasm/tests/markdown_wasm.rs`
- Modify: `Cargo.toml` (workspace deps)

- [ ] **Step 1: Add `serde-wasm-bindgen` to workspace deps**

In root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
serde-wasm-bindgen = "0.6"
```

- [ ] **Step 2: Add deps to `crates/sunset-web-wasm/Cargo.toml`**

In the `[dependencies]` block, add:

```toml
sunset-markdown = { workspace = true, features = ["serde"] }
```

In the `[target.'cfg(target_arch = "wasm32")'.dependencies]` block, add:

```toml
serde-wasm-bindgen = { workspace = true }
```

(`serde` is already pulled in transitively through other crates; verify with `cargo tree` if unsure, otherwise add it explicitly to `[dependencies]` as `serde = { workspace = true, features = ["derive"] }`.)

- [ ] **Step 3: Create `crates/sunset-web-wasm/src/markdown.rs`**

```rust
//! WASM export for `sunset_markdown::parse`. Returns the parsed
//! `Document` to JS as a structured value via `serde-wasm-bindgen`.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn parse_markdown(input: &str) -> JsValue {
    let doc = sunset_markdown::parse(input);
    serde_wasm_bindgen::to_value(&doc).expect("AST is plain data; serialization cannot fail")
}
```

- [ ] **Step 4: Wire the module into `lib.rs`**

In `crates/sunset-web-wasm/src/lib.rs`, add (next to the other `#[cfg(target_arch = "wasm32")]` mod declarations):

```rust
#[cfg(target_arch = "wasm32")]
mod markdown;
#[cfg(target_arch = "wasm32")]
pub use markdown::parse_markdown;
```

The `#[wasm_bindgen]` macro itself handles the JS-side export. The `pub use` makes the function callable from a Rust integration test in Step 6.

- [ ] **Step 5: Verify the workspace still builds for the host target**

```
nix develop --command cargo build --workspace
```

Expected: success.

- [ ] **Step 6: Write the WASM test**

Create `crates/sunset-web-wasm/tests/markdown_wasm.rs`:

```rust
//! Round-trip `parse_markdown` through wasm-bindgen so a `serde` derive
//! drift in the AST shape is caught at CI time, not at first user load.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn parse_markdown_returns_some_value() {
    let value = sunset_web_wasm::parse_markdown("hello");
    assert!(!value.is_undefined());
    assert!(!value.is_null());
}

#[wasm_bindgen_test]
fn parse_markdown_round_trips_bold_text() {
    let value = sunset_web_wasm::parse_markdown("**hi**");
    let json = js_sys::JSON::stringify(&value)
        .expect("stringify")
        .as_string()
        .expect("stringify result");
    assert!(json.contains("\"Bold\""), "expected Bold variant in JSON, got: {json}");
    assert!(json.contains("\"hi\""), "expected payload, got: {json}");
}
```

- [ ] **Step 7: Add `js-sys` and `wasm-bindgen` to dev-deps if needed**

In `crates/sunset-web-wasm/Cargo.toml`, ensure the wasm32 dev-deps include `js-sys` and `wasm-bindgen`:

```toml
[target.'cfg(target_arch = "wasm32")'.dev-dependencies]
wasm-bindgen-test.workspace = true
js-sys.workspace = true
wasm-bindgen.workspace = true
```

(Check what's already there before adding duplicates.)

- [ ] **Step 8: Run the wasm test**

```
nix develop --command wasm-pack test --node crates/sunset-web-wasm
```

(If the project uses a different runner — e.g. `cargo test --target wasm32-unknown-unknown` — adapt accordingly. Look at how existing wasm-bindgen tests in the workspace are run, e.g. in CI configuration or `flake.nix`.)

Expected: 2 tests passing.

If `wasm-pack` isn't available in the flake, add it to `flake.nix`'s `devShells.default.buildInputs` per the hermeticity rule (CLAUDE.md), commit that change, and retry.

- [ ] **Step 9: Commit**

```
git add Cargo.toml crates/sunset-web-wasm flake.nix flake.lock
git commit -m "sunset-web-wasm: parse_markdown export with wasm-bindgen-test

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

(Drop `flake.nix flake.lock` if you didn't change them.)

---

## Phase C — Web markdown renderer

The web app calls `parse_markdown` over the existing FFI bridge, walks the JS AST, and returns Lustre `Element` nodes.

### Task C1: FFI shim and Gleam scaffolding

**Files:**
- Modify: `web/src/sunset_web/sunset.ffi.mjs`
- Create: `web/src/sunset_web/markdown.gleam`
- Create: `web/src/sunset_web/markdown.ffi.mjs`

- [ ] **Step 1: Add the JS FFI export**

Create `web/src/sunset_web/markdown.ffi.mjs`:

```js
// Bridges sunset-web-wasm's parse_markdown to Gleam.
//
// `Document(pub Vec<Block>)` is a serde transparent newtype, so the
// JSON shape is just a JS array of blocks: [block, block, ...].
//
// Each Block is one of (externally tagged):
//
//   { Paragraph: [inline, ...] }
//   { Heading: { level: "H1"|"H2"|"H3", content: [inline, ...] } }
//   { Quote: [block, ...] }
//   { UnorderedList: [[block, ...], ...] }
//   { CodeBlock: { language: string|null, source: string } }
//
// Each Inline is one of:
//
//   { Text: "..." } | { Bold: [...] } | { Italic: [...] }
//   | { Underline: [...] } | { Strikethrough: [...] } | { Spoiler: [...] }
//   | { InlineCode: "..." }
//   | { Link: { label: [inline, ...], url: "...", autolink: true|false } }
//   | "LineBreak"   // unit variant — serialized as a bare string
//
// On any error (e.g. WASM not yet loaded), returns the body as a single
// Paragraph(Text(body)) so rendering still produces something useful.

import { parse_markdown } from "../../sunset_web_wasm.js";

export function parseMarkdown(body) {
  try {
    return parse_markdown(body);
  } catch (err) {
    console.error("markdown.parseMarkdown failed:", err);
    return [{ Paragraph: [{ Text: body }] }];
  }
}
```

- [ ] **Step 2: Create `markdown.gleam` scaffolding**

Create `web/src/sunset_web/markdown.gleam`:

```gleam
//// Render a message body string to a Lustre element by parsing it as
//// Discord-flavored markdown. The parser lives in the Rust
//// `sunset-markdown` crate and is reached through `markdown.ffi.mjs`.
////
//// The AST shape is an opaque JS value from Gleam's perspective. We
//// treat it as `dynamic.Dynamic` and decode it with the
//// `gleam/dynamic/decode` API used elsewhere in the codebase.

import gleam/dynamic.{type Dynamic}
import lustre/element.{type Element}
import lustre/element/html

import sunset_web/theme.{type Palette}

@external(javascript, "./markdown.ffi.mjs", "parseMarkdown")
fn parse_markdown_ffi(body: String) -> Dynamic

/// Render the body to a single block-container `Element`. Subsequent
/// tasks fill in the block/inline cases — for now everything renders
/// as plain text.
pub fn render(body: String, _p: Palette) -> Element(msg) {
  let _ast = parse_markdown_ffi(body)
  // Phase C1: stub. Replaced by Tasks C2..C5.
  html.div([], [html.text(body)])
}

/// Strip all formatting and return concatenated text. Useful for
/// notification bodies and `aria-label`s.
pub fn to_plain(body: String) -> String {
  // Phase C1: stub. Replaced by Task C6.
  body
}
```

- [ ] **Step 3: Verify the Gleam code compiles**

From `web/`:

```
nix develop --command gleam build
```

Expected: success.

- [ ] **Step 4: Commit**

```
git add web/src/sunset_web/markdown.gleam web/src/sunset_web/markdown.ffi.mjs
git commit -m "sunset_web: markdown render scaffolding

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C2: Render inline nodes (text, bold, italic, underline, strikethrough, code, line break)

This task replaces the stub `render` body with a real walker that handles the simpler inline variants. Spoiler and Link come in Tasks C3–C4.

**Files:**
- Modify: `web/src/sunset_web/markdown.gleam`
- Create: `web/test/markdown_test.gleam`

- [ ] **Step 1: Write failing tests**

Create `web/test/markdown_test.gleam`:

```gleam
import gleam/string
import gleeunit/should
import lustre/element
import sunset_web/markdown
import sunset_web/theme

pub fn render_plain_text_test() {
  // We can't easily compare Lustre elements structurally, so we render
  // to an HTML string and assert on substrings. (Exact attribute order
  // and quoting differs between Lustre versions; lock in semantics, not
  // a specific serialization.)
  let html =
    markdown.render("hello", theme.dark())
    |> element.to_string()
  should.be_true(string.contains(html, "<p"))
  should.be_true(string.contains(html, "hello"))
}

pub fn render_bold_test() {
  let html =
    markdown.render("a **b** c", theme.dark())
    |> element.to_string()
  should.be_true(string.contains(html, "<strong>b</strong>"))
  should.be_true(string.contains(html, "a "))
  should.be_true(string.contains(html, " c"))
}

pub fn render_italic_test() {
  let html =
    markdown.render("a *b* c", theme.dark())
    |> element.to_string()
  should.be_true(string.contains(html, "<em>b</em>"))
}

pub fn render_inline_code_test() {
  let html =
    markdown.render("a `b` c", theme.dark())
    |> element.to_string()
  should.be_true(string.contains(html, "<code"))
  should.be_true(string.contains(html, ">b</code>"))
}

pub fn render_line_break_test() {
  let html =
    markdown.render("a\nb", theme.dark())
    |> element.to_string()
  should.be_true(string.contains(html, "<br"))
  should.be_true(string.contains(html, "a"))
  should.be_true(string.contains(html, "b"))
}
```

(`theme.dark()` is defined in `web/src/sunset_web/theme.gleam`; check there if anything looks off.)

- [ ] **Step 2: Run tests to verify they fail**

From `web/`:

```
nix develop --command gleam test
```

Expected: 5 failures from `markdown_test`.

- [ ] **Step 3: Implement the inline walker**

Update imports at the top of `markdown.gleam`:

```gleam
import gleam/dynamic.{type Dynamic}
import gleam/dynamic/decode
import gleam/list
import gleam/option.{type Option}
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html

import sunset_web/theme.{type Palette}
```

Replace the body of `markdown.gleam` (everything after the imports and the `parse_markdown_ffi` external) with:

```gleam
@external(javascript, "./markdown.ffi.mjs", "parseMarkdown")
fn parse_markdown_ffi(body: String) -> Dynamic

pub fn render(body: String, p: Palette) -> Element(msg) {
  let ast = parse_markdown_ffi(body)
  let blocks = case decode.run(ast, decode.list(block_decoder())) {
    Ok(bs) -> bs
    Error(_) -> [Paragraph([Text(body)])]
  }
  html.div([], list.map(blocks, fn(b) { render_block(b, p) }))
}

// ----- AST types -----

type Block {
  Paragraph(content: List(Inline))
  Heading(level: Int, content: List(Inline))
  Quote(content: List(Block))
  UnorderedList(items: List(List(Block)))
  CodeBlock(language: Option(String), source: String)
}

type Inline {
  Text(value: String)
  Bold(children: List(Inline))
  Italic(children: List(Inline))
  Underline(children: List(Inline))
  Strikethrough(children: List(Inline))
  Spoiler(children: List(Inline))
  InlineCode(value: String)
  Link(label: List(Inline), url: String, autolink: Bool)
  LineBreak
}

// ----- Decoders -----
//
// Externally-tagged enums from serde-wasm-bindgen come through as
// either:
//   - {"VariantName": payload}    (variants with data)
//   - "VariantName"               (unit variants like LineBreak)
//
// `decode.one_of` tries each branch in order. We use a dedicated tag
// decoder per variant.

fn block_decoder() -> decode.Decoder(Block) {
  decode.one_of(paragraph_decoder(), [
    heading_decoder(),
    quote_decoder(),
    unordered_list_decoder(),
    code_block_decoder(),
  ])
}

fn paragraph_decoder() -> decode.Decoder(Block) {
  use inlines <- decode.field("Paragraph", decode.list(inline_decoder()))
  decode.success(Paragraph(inlines))
}

fn heading_decoder() -> decode.Decoder(Block) {
  use payload <- decode.field("Heading", heading_payload_decoder())
  decode.success(payload)
}

fn heading_payload_decoder() -> decode.Decoder(Block) {
  use level <- decode.field("level", decode.string)
  use content <- decode.field("content", decode.list(inline_decoder()))
  let n = case level {
    "H1" -> 1
    "H2" -> 2
    _ -> 3
  }
  decode.success(Heading(n, content))
}

fn quote_decoder() -> decode.Decoder(Block) {
  use blocks <- decode.field("Quote", decode.list(block_decoder()))
  decode.success(Quote(blocks))
}

fn unordered_list_decoder() -> decode.Decoder(Block) {
  use items <- decode.field(
    "UnorderedList",
    decode.list(decode.list(block_decoder())),
  )
  decode.success(UnorderedList(items))
}

fn code_block_decoder() -> decode.Decoder(Block) {
  use payload <- decode.field("CodeBlock", code_block_payload_decoder())
  decode.success(payload)
}

fn code_block_payload_decoder() -> decode.Decoder(Block) {
  use language <- decode.field("language", decode.optional(decode.string))
  use source <- decode.field("source", decode.string)
  decode.success(CodeBlock(language, source))
}

fn inline_decoder() -> decode.Decoder(Inline) {
  decode.one_of(line_break_decoder(), [
    text_decoder(),
    bold_decoder(),
    italic_decoder(),
    underline_decoder(),
    strikethrough_decoder(),
    spoiler_decoder(),
    inline_code_decoder(),
    link_decoder(),
  ])
}

fn line_break_decoder() -> decode.Decoder(Inline) {
  use s <- decode.then(decode.string)
  case s {
    "LineBreak" -> decode.success(LineBreak)
    _ -> decode.failure(LineBreak, "not LineBreak")
  }
}

fn text_decoder() -> decode.Decoder(Inline) {
  use s <- decode.field("Text", decode.string)
  decode.success(Text(s))
}

fn inline_code_decoder() -> decode.Decoder(Inline) {
  use s <- decode.field("InlineCode", decode.string)
  decode.success(InlineCode(s))
}

fn bold_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Bold", decode.list(inline_decoder()))
  decode.success(Bold(xs))
}

fn italic_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Italic", decode.list(inline_decoder()))
  decode.success(Italic(xs))
}

fn underline_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Underline", decode.list(inline_decoder()))
  decode.success(Underline(xs))
}

fn strikethrough_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Strikethrough", decode.list(inline_decoder()))
  decode.success(Strikethrough(xs))
}

fn spoiler_decoder() -> decode.Decoder(Inline) {
  use xs <- decode.field("Spoiler", decode.list(inline_decoder()))
  decode.success(Spoiler(xs))
}

fn link_decoder() -> decode.Decoder(Inline) {
  use payload <- decode.field("Link", link_payload_decoder())
  decode.success(payload)
}

fn link_payload_decoder() -> decode.Decoder(Inline) {
  use label <- decode.field("label", decode.list(inline_decoder()))
  use url <- decode.field("url", decode.string)
  use autolink <- decode.field("autolink", decode.bool)
  decode.success(Link(label, url, autolink))
}

// ----- Block rendering -----

fn render_block(b: Block, p: Palette) -> Element(msg) {
  case b {
    Paragraph(inlines) ->
      html.p([], list.flat_map(inlines, fn(i) { render_inline(i, p) }))
    _ -> html.text("")
    // Block variants other than Paragraph filled in by Task C5.
  }
}

// ----- Inline rendering -----

fn render_inline(i: Inline, p: Palette) -> List(Element(msg)) {
  case i {
    Text(s) -> [html.text(s)]
    Bold(xs) -> [
      html.strong([], list.flat_map(xs, fn(x) { render_inline(x, p) })),
    ]
    Italic(xs) -> [html.em([], list.flat_map(xs, fn(x) { render_inline(x, p) }))]
    Underline(xs) -> [html.u([], list.flat_map(xs, fn(x) { render_inline(x, p) }))]
    Strikethrough(xs) -> [
      html.s([], list.flat_map(xs, fn(x) { render_inline(x, p) })),
    ]
    InlineCode(s) -> [
      html.code(
        [
          attribute.attribute(
            "style",
            "font-family: var(--font-mono); background: rgba(0,0,0,0.1); padding: 0 4px; border-radius: 3px;",
          ),
        ],
        [html.text(s)],
      ),
    ]
    LineBreak -> [html.br([])]
    Spoiler(_xs) -> [html.text("")]
    // Spoiler rendering in Task C3.
    Link(_, _, _) -> [html.text("")]
    // Link rendering in Task C4.
  }
}
```

If the Gleam compiler warns about unused `option.Option` (it's only used in the `CodeBlock` type at this stage, and that variant isn't constructed yet), add `option.None` as a placeholder in some future-task TODO comment, or just ignore the warning — Tasks C5 and C6 consume it.

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command gleam test
```

Expected: 5 markdown tests passing (plus whatever was passing before).

If the InlineCode test fails because `var(--font-mono)` differs from your project's mono-font binding, look at how `theme.gleam` exposes the mono font and use that token instead. The exact CSS string isn't load-bearing — it's a stylistic choice.

- [ ] **Step 5: Commit**

```
git add web/src/sunset_web/markdown.gleam web/test/markdown_test.gleam
git commit -m "sunset_web: render plain inline markdown nodes

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C3: Render spoiler nodes with reveal state

Spoilers need shared model state so a click toggles "revealed" globally for that span. State lives on the top-level `Model` in `sunset_web.gleam` as a `Set(#(String, Int))` keyed by `(message_id, offset_in_body)`. Resets when the user navigates away from the room.

**Files:**
- Modify: `web/src/sunset_web/markdown.gleam`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Plumb spoiler state through `render`**

Add to the imports at the top of `markdown.gleam`:

```gleam
import gleam/int
import lustre/event
```

Add a `SpoilerKey` type and a `Ctx` type just below the existing `Inline` type:

```gleam
pub type SpoilerKey {
  SpoilerKey(message_id: String, offset: Int)
}

type Ctx(msg) {
  Ctx(
    palette: Palette,
    message_id: String,
    is_revealed: fn(SpoilerKey) -> Bool,
    on_toggle: fn(SpoilerKey) -> msg,
  )
}
```

Replace the existing `render`, `render_block`, and `render_inline` functions with versions that thread `Ctx` and an offset counter:

```gleam
pub fn render(
  body: String,
  message_id: String,
  is_spoiler_revealed: fn(SpoilerKey) -> Bool,
  on_toggle_spoiler: fn(SpoilerKey) -> msg,
  p: Palette,
) -> Element(msg) {
  let ast = parse_markdown_ffi(body)
  let blocks = case decode.run(ast, decode.list(block_decoder())) {
    Ok(bs) -> bs
    Error(_) -> [Paragraph([Text(body)])]
  }
  let ctx =
    Ctx(
      palette: p,
      message_id: message_id,
      is_revealed: is_spoiler_revealed,
      on_toggle: on_toggle_spoiler,
    )
  html.div(
    [],
    list.index_map(blocks, fn(b, i) { render_block(b, ctx, i * 1_000_000) }),
  )
}

fn render_block(b: Block, ctx: Ctx(msg), offset: Int) -> Element(msg) {
  case b {
    Paragraph(inlines) -> html.p([], render_inlines(inlines, ctx, offset))
    _ -> html.text("")
  }
}

fn render_inlines(
  is: List(Inline),
  ctx: Ctx(msg),
  offset_base: Int,
) -> List(Element(msg)) {
  list.index_map(is, fn(i, idx) { render_inline(i, ctx, offset_base + idx) })
  |> list.flatten()
}

fn render_inline(i: Inline, ctx: Ctx(msg), offset: Int) -> List(Element(msg)) {
  case i {
    Text(s) -> [html.text(s)]
    Bold(xs) -> [html.strong([], render_inlines(xs, ctx, offset * 100))]
    Italic(xs) -> [html.em([], render_inlines(xs, ctx, offset * 100))]
    Underline(xs) -> [html.u([], render_inlines(xs, ctx, offset * 100))]
    Strikethrough(xs) -> [html.s([], render_inlines(xs, ctx, offset * 100))]
    InlineCode(s) -> [
      html.code(
        [
          attribute.attribute(
            "style",
            "font-family: var(--font-mono); background: rgba(0,0,0,0.1); padding: 0 4px; border-radius: 3px;",
          ),
        ],
        [html.text(s)],
      ),
    ]
    LineBreak -> [html.br([])]
    Spoiler(xs) -> [render_spoiler(xs, ctx, offset)]
    Link(_, _, _) -> [html.text("")]
    // Link rendering in Task C4.
  }
}

fn render_spoiler(
  xs: List(Inline),
  ctx: Ctx(msg),
  offset: Int,
) -> Element(msg) {
  let key = SpoilerKey(ctx.message_id, offset)
  let revealed = ctx.is_revealed(key)
  let style = case revealed {
    True -> "background: rgba(0,0,0,0.05); border-radius: 3px; padding: 0 2px;"
    False ->
      "background: var(--text-muted, #888); color: transparent; border-radius: 3px; padding: 0 2px; cursor: pointer; user-select: none;"
  }
  html.span(
    [
      attribute.class("spoiler"),
      attribute.attribute("data-msg-id", ctx.message_id),
      attribute.attribute("data-offset", int.to_string(offset)),
      attribute.attribute("style", style),
      event.on_click(ctx.on_toggle(key)),
    ],
    render_inlines(xs, ctx, offset * 100),
  )
}
```

- [ ] **Step 2: Update existing tests to pass the new arguments**

In `web/test/markdown_test.gleam`, change every `markdown.render(body, theme.dark())` call to:

```gleam
markdown.render(
  "hello",
  "msg-id",
  fn(_) { False },
  fn(_) { Nil },
  theme.dark(),
)
```

(The closure `fn(_) { Nil }` returns `Nil` to act as the `msg` placeholder — `Element(Nil)` is fine in tests.)

- [ ] **Step 3: Add a test for the spoiler reveal state**

In `web/test/markdown_test.gleam`, append:

```gleam
pub fn render_spoiler_hidden_test() {
  let html =
    markdown.render(
      "||secret||",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()

  // Hidden spoilers have color: transparent — quick substring check.
  case string.contains(html, "color: transparent") {
    True -> Nil
    False -> panic as "expected hidden spoiler to have color: transparent"
  }
}

pub fn render_spoiler_revealed_test() {
  let html =
    markdown.render(
      "||secret||",
      "msg-1",
      fn(_) { True },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()

  case string.contains(html, "color: transparent") {
    False -> Nil
    True -> panic as "expected revealed spoiler to NOT have color: transparent"
  }
}
```

(Add `import gleam/string` at the top of the test module.)

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command gleam test
```

Expected: all markdown tests pass.

- [ ] **Step 5: Commit**

```
git add web/src/sunset_web/markdown.gleam web/test/markdown_test.gleam
git commit -m "sunset_web: spoiler render with reveal state

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C4: Render link nodes with scheme allowlist

Allowlist: `http`, `https`, `mailto`. Disallowed → render as plain text. For autolinks, omit the `title` (the visible text is the URL).

**Files:**
- Modify: `web/src/sunset_web/markdown.gleam`
- Modify: `web/test/markdown_test.gleam`

- [ ] **Step 1: Write failing tests**

In `web/test/markdown_test.gleam`, append:

```gleam
pub fn render_masked_link_test() {
  let html =
    markdown.render(
      "[click](https://example.com)",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
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
  let html =
    markdown.render(
      "see https://example.com",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  should.be_true(string.contains(html, "<a "))
  should.be_true(string.contains(html, "href=\"https://example.com\""))
  should.be_false(string.contains(html, "title="))
}

pub fn render_disallowed_scheme_renders_as_text_test() {
  let html =
    markdown.render(
      "[bad](javascript:alert(1))",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  // No <a> tag, but the URL must still be visible somewhere.
  should.be_false(string.contains(html, "<a "))
  should.be_true(string.contains(html, "javascript:"))
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command gleam test
```

Expected: 3 new failures.

- [ ] **Step 3: Implement Link rendering**

Replace the `Link(_, _, _) -> [html.text("")]` branch in `render_inline` with:

```gleam
    Link(label, url, autolink) -> [render_link(label, url, autolink, ctx, offset)]
```

Add the helper:

```gleam
fn render_link(
  label: List(Inline),
  url: String,
  autolink: Bool,
  ctx: Ctx(msg),
  offset: Int,
) -> Element(msg) {
  case allowed_scheme(url) {
    True -> {
      let base_attrs = [
        attribute.href(url),
        attribute.target("_blank"),
        attribute.rel("noopener noreferrer"),
      ]
      let attrs = case autolink {
        True -> base_attrs
        False -> [attribute.title(url), ..base_attrs]
      }
      html.a(attrs, render_inlines(label, ctx, offset * 100))
    }
    False -> {
      // Render as plain text: label + " (" + url + ")" so the user can
      // still see what was sent.
      html.span(
        [],
        list.append(
          render_inlines(label, ctx, offset * 100),
          [html.text(" (" <> url <> ")")],
        ),
      )
    }
  }
}

fn allowed_scheme(url: String) -> Bool {
  string.starts_with(url, "http://")
  || string.starts_with(url, "https://")
  || string.starts_with(url, "mailto:")
}
```

(For an autolink the label *is* the URL, so the disallowed-scheme `(url)` suffix would duplicate the text. But autolinks only fire on `http(s)://`, so that branch never reaches the disallowed path. Fine to leave the helper symmetric.)

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command gleam test
```

Expected: all markdown tests pass.

If the test assertion strings don't quite match Lustre's HTML output (e.g. attribute order differs), update the assertion strings to match what Lustre actually produces — the *behavior* is what matters, and the test's job is to lock it in once you observe it. Re-run after each adjustment.

- [ ] **Step 5: Commit**

```
git add web/src/sunset_web/markdown.gleam web/test/markdown_test.gleam
git commit -m "sunset_web: render links with scheme allowlist

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C5: Render block nodes (heading, quote, list, code block)

**Files:**
- Modify: `web/src/sunset_web/markdown.gleam`
- Modify: `web/test/markdown_test.gleam`

- [ ] **Step 1: Write failing tests**

In `web/test/markdown_test.gleam`, append:

```gleam
pub fn render_heading_test() {
  let html =
    markdown.render(
      "# title\n\nbody",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  case string.contains(html, "<h1") && string.contains(html, "title") {
    True -> Nil
    False -> panic as "expected <h1> with 'title' content"
  }
}

pub fn render_quote_test() {
  let html =
    markdown.render(
      "> hello",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  case string.contains(html, "<blockquote") {
    True -> Nil
    False -> panic as "expected <blockquote>"
  }
}

pub fn render_unordered_list_test() {
  let html =
    markdown.render(
      "- one\n- two",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  case string.contains(html, "<ul") && string.contains(html, "<li") {
    True -> Nil
    False -> panic as "expected <ul> with <li>"
  }
}

pub fn render_code_block_with_language_test() {
  let html =
    markdown.render(
      "```rust\nfn main() {}\n```",
      "msg-1",
      fn(_) { False },
      fn(_) { Nil },
      theme.dark(),
    )
    |> element.to_string()
  case
    string.contains(html, "<pre"),
    string.contains(html, "<code"),
    string.contains(html, "rust"),
    string.contains(html, "fn main()")
  {
    True, True, True, True -> Nil
    _, _, _, _ -> panic as "expected <pre><code> containing language pill 'rust' and source"
  }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
nix develop --command gleam test
```

Expected: 4 new failures.

- [ ] **Step 3: Implement block rendering**

Replace the `case b { ... }` body in `render_block` with:

```gleam
fn render_block(b: Block, ctx: Ctx(msg), offset: Int) -> Element(msg) {
  case b {
    Paragraph(inlines) ->
      html.p([], render_inlines(inlines, ctx, offset))

    Heading(level, content) -> {
      let tag = case level {
        1 -> html.h1
        2 -> html.h2
        _ -> html.h3
      }
      tag([], render_inlines(content, ctx, offset))
    }

    Quote(blocks) ->
      html.blockquote(
        [
          attribute.attribute(
            "style",
            "border-left: 3px solid var(--border, #888); padding-left: 8px; color: var(--text-muted, inherit); margin: 0;",
          ),
        ],
        list.index_map(blocks, fn(b, i) { render_block(b, ctx, offset * 100 + i) }),
      )

    UnorderedList(items) ->
      html.ul(
        [],
        list.index_map(items, fn(item, i) {
          html.li(
            [],
            list.index_map(item, fn(b, j) {
              render_block(b, ctx, offset * 100 + i * 10 + j)
            }),
          )
        }),
      )

    CodeBlock(language, source) -> render_code_block(language, source, ctx.palette)
  }
}

fn render_code_block(
  language: Option(String),
  source: String,
  _p: Palette,
) -> Element(msg) {
  let pill = case language {
    option.Some(lang) -> [
      html.span(
        [
          attribute.attribute(
            "style",
            "position: absolute; top: 4px; right: 8px; font-size: 10px; text-transform: uppercase; opacity: 0.6;",
          ),
        ],
        [html.text(lang)],
      ),
    ]
    option.None -> []
  }
  html.div(
    [
      attribute.attribute(
        "style",
        "position: relative; background: rgba(0,0,0,0.06); border-radius: 6px; padding: 8px 12px; margin: 4px 0;",
      ),
    ],
    list.append(pill, [
      html.pre(
        [attribute.attribute("style", "margin: 0; white-space: pre-wrap;")],
        [
          html.code(
            [attribute.attribute("style", "font-family: var(--font-mono);")],
            [html.text(source)],
          ),
        ],
      ),
    ]),
  )
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
nix develop --command gleam test
```

Expected: all markdown tests pass.

- [ ] **Step 5: Commit**

```
git add web/src/sunset_web/markdown.gleam web/test/markdown_test.gleam
git commit -m "sunset_web: render heading, quote, list, code block

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C6: Implement `to_plain` Gleam helper

This mirrors `sunset_markdown::to_plain` on the Gleam side so callers don't need to round-trip through WASM just to strip formatting (we can call WASM `to_plain` later if we add the export).

For now, the simplest correct implementation is to delegate to the Rust side via a new `to_plain_markdown` FFI export. This keeps the two implementations in sync at a single source of truth.

**Files:**
- Modify: `crates/sunset-markdown/src/lib.rs` (already has `to_plain`)
- Modify: `crates/sunset-web-wasm/src/markdown.rs`
- Modify: `web/src/sunset_web/markdown.ffi.mjs`
- Modify: `web/src/sunset_web/markdown.gleam`
- Modify: `web/test/markdown_test.gleam`

- [ ] **Step 1: Add `to_plain_markdown` WASM export**

In `crates/sunset-web-wasm/src/markdown.rs`, append:

```rust
#[wasm_bindgen]
pub fn to_plain_markdown(input: &str) -> String {
    sunset_markdown::to_plain(&sunset_markdown::parse(input))
}
```

- [ ] **Step 2: Add JS FFI export**

In `web/src/sunset_web/markdown.ffi.mjs`, append (and update the import line):

```js
import { parse_markdown, to_plain_markdown } from "../../sunset_web_wasm.js";

export function toPlain(body) {
  try {
    return to_plain_markdown(body);
  } catch (err) {
    console.error("markdown.toPlain failed:", err);
    return body;
  }
}
```

- [ ] **Step 3: Replace the stub `to_plain` in `markdown.gleam`**

```gleam
@external(javascript, "./markdown.ffi.mjs", "toPlain")
pub fn to_plain(body: String) -> String
```

(You can delete the old `pub fn to_plain(body: String) -> String { body }` body.)

- [ ] **Step 4: Add a Gleam test**

In `web/test/markdown_test.gleam`, append:

```gleam
pub fn to_plain_strips_bold_test() {
  markdown.to_plain("hello **bold** _italic_")
  |> should.equal("hello bold italic")
}
```

- [ ] **Step 5: Run tests**

Build and run:

```
nix develop --command cargo build --workspace
nix develop --command gleam test
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```
git add crates/sunset-web-wasm/src/markdown.rs web/src/sunset_web/markdown.ffi.mjs web/src/sunset_web/markdown.gleam web/test/markdown_test.gleam
git commit -m "sunset_web: to_plain via WASM bridge

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C7: Add spoiler reveal state to top-level model + ToggleSpoiler msg

**Files:**
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Identify the model + Msg type definitions**

Open `web/src/sunset_web.gleam`. Around line 60–130 you'll find the `Model` type and the `Msg` (a.k.a. `UpdateLandingInput(...)` etc.) type. Read them so subsequent edits make sense in context.

- [ ] **Step 2: Add the field to `Model`**

In the `Model` record, add a field:

```gleam
revealed_spoilers: set.Set(#(String, Int)),
```

(Add `import gleam/set` at the top if not already imported.)

- [ ] **Step 3: Initialize the field**

In every place the `Model` is constructed (search for `Model(`), pass `revealed_spoilers: set.new()` for fresh state. There are at least two: the initial model around line 218 and the room-switch around line 463 (which should also reset the set — re-entering the room hides spoilers again).

- [ ] **Step 4: Add the message variant**

In the message ADT (search for `UpdateLandingInput(...)` to find the type), add:

```gleam
ToggleSpoiler(message_id: String, offset: Int)
```

- [ ] **Step 5: Handle the message**

In the `update` function (search for `UpdateLandingInput(s) ->`), add:

```gleam
ToggleSpoiler(mid, off) -> {
  let key = #(mid, off)
  let next = case set.contains(model.revealed_spoilers, key) {
    True -> set.delete(model.revealed_spoilers, key)
    False -> set.insert(model.revealed_spoilers, key)
  }
  #(Model(..model, revealed_spoilers: next), effect.none())
}
```

- [ ] **Step 6: Verify the project still compiles**

```
nix develop --command gleam build
```

Expected: success.

- [ ] **Step 7: Commit**

```
git add web/src/sunset_web.gleam
git commit -m "sunset_web: spoiler reveal state and ToggleSpoiler msg

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task C8: Wire `markdown.render` into the message stream

Replace the existing `html.text(m.body)` at `views/main_panel.gleam:292` with `markdown.render(m.body, m.id, is_revealed, on_toggle, p)`.

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web.gleam` (to pass spoiler args into main_panel)

- [ ] **Step 1: Find the call site**

Open `views/main_panel.gleam` and find the message-row rendering. The body div at line ~283-293 currently looks like:

```gleam
html.div(
  [
    ui.css([
      #("font-size", "16.875px"),
      #("color", p.text),
      #("white-space", "pre-wrap"),
      #("word-break", "break-word"),
    ]),
  ],
  [html.text(m.body)],
),
```

- [ ] **Step 2: Identify the function that renders the message row**

Trace upward to find which function contains this, and see whether it already takes the model's spoiler state. It probably doesn't. Add the parameters to that function and to its callers.

The fastest way: temporarily change `[html.text(m.body)]` to:

```gleam
[markdown.render(m.body, m.id, is_revealed, on_toggle_spoiler, p)],
```

…then add `is_revealed: fn(markdown.SpoilerKey) -> Bool` and `on_toggle_spoiler: fn(markdown.SpoilerKey) -> msg` parameters to the enclosing function and to its caller (the `messages_view`/main_panel entrypoint), all the way up to `sunset_web.view`. Add the `import sunset_web/markdown` at the top of `main_panel.gleam`.

- [ ] **Step 3: Wire the spoiler state at the top level**

In `web/src/sunset_web.gleam`'s `view` function (search for `main_panel.view(`), pass:

```gleam
is_revealed: fn(k) {
  set.contains(
    model.revealed_spoilers,
    #(k.message_id, k.offset),
  )
},
on_toggle_spoiler: fn(k) { ToggleSpoiler(k.message_id, k.offset) },
```

(The exact field name for the SpoilerKey constructor is `message_id` and `offset`, per `markdown.SpoilerKey`.)

- [ ] **Step 4: Build and run tests**

```
nix develop --command gleam build
nix develop --command gleam test
```

Expected: success.

- [ ] **Step 5: Manual smoke check**

If the lustre dev server is part of the workflow, start it and verify a message containing `**bold** [link](https://example.com)` renders correctly. If you can't easily run the dev server, skip this step — Task E1 covers it via Playwright.

- [ ] **Step 6: Commit**

```
git add web/src/sunset_web.gleam web/src/sunset_web/views/main_panel.gleam
git commit -m "sunset_web: render message body via markdown

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase D — Composer

The composer becomes a textarea with auto-grow, Enter-sends, Shift+Enter-newline, and three keyboard shortcuts.

### Task D1: Convert composer to `<textarea>` with auto-grow

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Create: `web/src/sunset_web/composer.ffi.mjs`
- Create: `web/src/sunset_web/composer.gleam`

- [ ] **Step 1: Create the FFI helper**

Create `web/src/sunset_web/composer.ffi.mjs`:

```js
// Composer DOM helpers — auto-grow on input + selection-aware
// template insertion for keyboard shortcuts.

const MAX_LINES = 10;

export function autoGrow(elementId) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return;
  // Reset, measure, cap.
  el.style.height = "auto";
  const lineHeight = parseFloat(getComputedStyle(el).lineHeight) || 20;
  const maxHeight = lineHeight * MAX_LINES;
  el.style.height = Math.min(el.scrollHeight, maxHeight) + "px";
  el.style.overflowY = el.scrollHeight > maxHeight ? "auto" : "hidden";
}

export function applyTemplate(elementId, before, between, after, caretAtBetween) {
  const el = document.getElementById(elementId);
  if (!el || el.tagName !== "TEXTAREA") return el ? el.value : "";
  const start = el.selectionStart;
  const end = el.selectionEnd;
  const selected = el.value.slice(start, end);
  const middle = selected.length > 0 ? selected : between;
  const replacement = before + middle + after;
  el.value = el.value.slice(0, start) + replacement + el.value.slice(end);
  // Place caret.
  const caret =
    caretAtBetween
      ? start + before.length + middle.length
      : start + before.length;
  el.selectionStart = caret;
  el.selectionEnd = selected.length > 0 ? caret : caret;
  // Re-fire input so Gleam's on_input handler updates the model.
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.focus();
  return el.value;
}
```

- [ ] **Step 2: Create the Gleam wrapper**

Create `web/src/sunset_web/composer.gleam`:

```gleam
//// Thin wrappers around `composer.ffi.mjs`. Used by main_panel.

@external(javascript, "./composer.ffi.mjs", "autoGrow")
pub fn auto_grow(element_id: String) -> Nil

@external(javascript, "./composer.ffi.mjs", "applyTemplate")
pub fn apply_template(
  element_id: String,
  before: String,
  between: String,
  after: String,
  caret_at_between: Bool,
) -> String
```

- [ ] **Step 3: Replace `html.input` with `html.textarea` in `composer`**

In `views/main_panel.gleam`'s `composer` fn (around line 728), replace the `html.input([...])` call (lines 772–793) with:

```gleam
html.textarea(
  [
    attribute.id("composer-textarea"),
    attribute.autofocus(True),
    attribute.placeholder("Message #" <> channel_name),
    attribute.attribute("rows", "1"),
    event.on_input(on_draft),
    event.on("keydown", {
      use key <- decode.subfield(["key"], decode.string)
      use shift <- decode.subfield(["shiftKey"], decode.bool)
      decode.success(case key, shift {
        "Enter", False -> on_submit
        _, _ -> noop
      })
    }),
    ui.css([
      #("flex", "1"),
      #("border", "none"),
      #("background", "transparent"),
      #("font-family", "inherit"),
      #("font-size", "16.25px"),
      #("color", p.text),
      #("outline", "none"),
      #("resize", "none"),
      #("overflow", "hidden"),
      #("padding", "0"),
      #("line-height", "1.4"),
    ]),
  ],
  [html.text(draft)],
)
```

Note: `<textarea>` doesn't support `attribute.value` — the value is the inner text. Lustre re-syncs this on each render, which is what we want.

- [ ] **Step 4: Trigger `auto_grow` after each input**

In the same composer fn, change the `on_draft` wiring to also call `auto_grow`. Since Gleam's `event.on_input` only takes a `String -> msg`, and we want the side effect to happen *after* the model update, the cleanest approach is to wire it into the render path: a Lustre `effect` that runs `composer.auto_grow("composer-textarea")` after each render where `draft` changed.

The simplest implementation: call `auto_grow` from inside the existing `update` for `UpdateDraft` (or whatever the message is named) by returning an `effect` that calls it. Search `update` for the draft handler and change:

```gleam
UpdateDraft(s) -> #(Model(..model, draft: s), effect.none())
```

to:

```gleam
UpdateDraft(s) -> #(
  Model(..model, draft: s),
  effect.from(fn(_dispatch) { composer.auto_grow("composer-textarea") }),
)
```

(Add `import sunset_web/composer` to `sunset_web.gleam`.)

- [ ] **Step 5: Build and verify it compiles**

```
nix develop --command gleam build
```

Expected: success.

- [ ] **Step 6: Commit**

```
git add web/src/sunset_web/composer.ffi.mjs web/src/sunset_web/composer.gleam web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam
git commit -m "sunset_web: composer textarea with auto-grow

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task D2: Cmd/Ctrl+B/I/K keyboard shortcuts

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web.gleam`

- [ ] **Step 1: Add `ApplyShortcut` to the message ADT**

In `web/src/sunset_web.gleam`, add a variant to the message type:

```gleam
ApplyComposerShortcut(before: String, between: String, after: String, caret_at_between: Bool)
```

In `update`, handle it:

```gleam
ApplyComposerShortcut(b, m, a, caret) -> {
  let new_value = composer.apply_template("composer-textarea", b, m, a, caret)
  #(Model(..model, draft: new_value), effect.none())
}
```

- [ ] **Step 2: Wire keydown for B / I / K**

In `views/main_panel.gleam` `composer` fn, expand the existing keydown decoder. Replace it with a handler that decodes `key`, `shiftKey`, and `(metaKey || ctrlKey)`:

```gleam
event.on("keydown", {
  use key <- decode.subfield(["key"], decode.string)
  use shift <- decode.subfield(["shiftKey"], decode.bool)
  use meta <- decode.subfield(["metaKey"], decode.bool)
  use ctrl <- decode.subfield(["ctrlKey"], decode.bool)
  let mod = meta || ctrl
  decode.success(case key, shift, mod {
    "Enter", False, _ -> on_submit
    "b", _, True -> on_shortcut("**", "", "**", True)
    "B", _, True -> on_shortcut("**", "", "**", True)
    "i", _, True -> on_shortcut("*", "", "*", True)
    "I", _, True -> on_shortcut("*", "", "*", True)
    "k", _, True -> on_shortcut("[", "", "](url)", True)
    "K", _, True -> on_shortcut("[", "", "](url)", True)
    _, _, _ -> noop
  })
}),
```

You'll need to add a parameter `on_shortcut: fn(String, String, String, Bool) -> msg` to the `composer` fn signature (and threading it through whoever calls `composer`).

- [ ] **Step 3: Wire the constructor at the top-level view**

In `web/src/sunset_web.gleam`, where `main_panel.view(...)` is called (or wherever `composer` is invoked indirectly), pass:

```gleam
on_shortcut: fn(b, m, a, caret) { ApplyComposerShortcut(b, m, a, caret) },
```

- [ ] **Step 4: Prevent the browser's default for Cmd+B**

`Cmd/Ctrl+B` and `Cmd/Ctrl+I` already have browser default behaviors (toggle bold/italic in contenteditable elements; in a textarea they're usually no-ops, but `Cmd+B` opens the bookmarks sidebar in some browsers). Add `event.prevent_default()` semantics. In Gleam/Lustre this is done by emitting a different event handler shape — the simplest is to use `decode.then`/`event.advanced` or attach `onkeydown` via FFI. For v1, the simplest ergonomic option: catch the keydown in the FFI helper.

Add to `composer.ffi.mjs`:

```js
export function attachShortcutPreventDefault(elementId) {
  const el = document.getElementById(elementId);
  if (!el) return;
  el.addEventListener("keydown", (ev) => {
    const mod = ev.metaKey || ev.ctrlKey;
    if (!mod) return;
    const key = ev.key.toLowerCase();
    if (key === "b" || key === "i" || key === "k") {
      ev.preventDefault();
    }
  });
}
```

Wrap in Gleam:

```gleam
@external(javascript, "./composer.ffi.mjs", "attachShortcutPreventDefault")
pub fn attach_shortcut_prevent_default(element_id: String) -> Nil
```

Call it once after the composer mounts. The simplest place: in the existing `init` function (or wherever `effect.batch` of post-mount setup runs), append `composer.attach_shortcut_prevent_default("composer-textarea")` inside an `effect.from` that fires once.

- [ ] **Step 5: Build and verify it compiles**

```
nix develop --command gleam build
```

Expected: success.

- [ ] **Step 6: Commit**

```
git add web/src/sunset_web/composer.ffi.mjs web/src/sunset_web/composer.gleam web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam
git commit -m "sunset_web: composer Cmd/Ctrl+B/I/K shortcuts

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase E — End-to-end tests

### Task E1: Playwright e2e — rich message display

**Files:**
- Modify: existing Playwright test directory (likely `web/e2e/`)
- Create: `web/e2e/markdown.spec.js`

- [ ] **Step 1: Look at an existing Playwright test for the pattern**

```
nix develop --command bash -c "ls web/e2e/"
nix develop --command bash -c "head -80 web/e2e/$(ls web/e2e/ | head -1)"
```

Note the helper functions (joining a room, sending a message, locating the message stream).

- [ ] **Step 2: Write the test**

Create `web/e2e/markdown.spec.js` (adapt the helpers/imports to match what the existing tests use):

```js
import { test, expect } from "@playwright/test";
import { joinRoom, sendMessage } from "./helpers.js"; // adjust to actual helper module

test("renders bold, link, and inline code in a message", async ({ page }) => {
  await joinRoom(page, "markdown-test");
  await sendMessage(page, "**bold** [link](https://example.com) `code`");

  const stream = page.getByTestId("message-stream"); // adjust to actual selector
  await expect(stream.locator("strong", { hasText: "bold" })).toBeVisible();
  await expect(
    stream.locator("a[href='https://example.com'][target='_blank'][rel='noopener noreferrer']", {
      hasText: "link",
    }),
  ).toBeVisible();
  await expect(stream.locator("code", { hasText: "code" })).toBeVisible();
});
```

- [ ] **Step 3: Run the test**

```
nix develop --command bash -c "cd web && npx playwright test markdown.spec.js"
```

Expected: pass. If selectors don't match the current DOM, adjust them based on what's actually there.

- [ ] **Step 4: Commit**

```
git add web/e2e/markdown.spec.js
git commit -m "web/e2e: rich message display

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task E2: Playwright e2e — composer textarea + shortcuts

**Files:**
- Create: `web/e2e/composer.spec.js`

- [ ] **Step 1: Write the test**

Create `web/e2e/composer.spec.js`:

```js
import { test, expect } from "@playwright/test";
import { joinRoom } from "./helpers.js"; // adjust to actual helper module

test("Enter sends, Shift+Enter inserts newline", async ({ page }) => {
  await joinRoom(page, "composer-test");
  const composer = page.locator("#composer-textarea");

  await composer.fill("first");
  await composer.press("Enter");
  await expect(page.getByTestId("message-stream")).toContainText("first");

  await composer.fill("a");
  await composer.press("Shift+Enter");
  await composer.type("b");
  await expect(composer).toHaveValue("a\nb");

  await composer.press("Enter");
  await expect(page.getByTestId("message-stream")).toContainText("a");
  // Multiline body becomes one message; check for "b" too on a different line.
  await expect(page.getByTestId("message-stream")).toContainText("b");
});

test("Ctrl+B wraps selection with **", async ({ page }) => {
  await joinRoom(page, "composer-shortcut-test");
  const composer = page.locator("#composer-textarea");

  await composer.fill("hello");
  // Select "hello".
  await composer.press("Control+A");
  // Use Meta on macOS, Control elsewhere — Playwright's "ControlOrMeta"
  // shorthand handles both.
  await composer.press("ControlOrMeta+B");
  await expect(composer).toHaveValue("**hello**");
});
```

- [ ] **Step 2: Run the test**

```
nix develop --command bash -c "cd web && npx playwright test composer.spec.js"
```

Expected: pass. Adjust selectors / helpers as needed.

- [ ] **Step 3: Commit**

```
git add web/e2e/composer.spec.js
git commit -m "web/e2e: composer textarea + Ctrl+B shortcut

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Final verification

After all tasks above, run the full suite from the repo root to make sure nothing regressed:

```
nix develop --command cargo nextest run --workspace --all-features
nix develop --command cargo clippy --workspace --all-features --all-targets -- -D warnings
nix develop --command cargo fmt --all --check
nix develop --command bash -c "cd web && gleam test"
nix develop --command bash -c "cd web && npx playwright test"
```

All green = ready for code review.

---

## Notes on test patching

Per `CLAUDE.md`'s "Debugging discipline" section: **never** patch a test to make it pass when the underlying behavior would be unacceptable to a real user. If a test in this plan fails because the *implementation* is wrong, fix the implementation. If a test fails because the *spec* is ambiguous (e.g. a subtle `***x***` rendering choice), resolve it by amending the spec with a marked revision (date-tagged) and add a golden-file test that pins the chosen behavior.
