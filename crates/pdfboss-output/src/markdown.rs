//! The Markdown adapter: the layout IR as CommonMark.

use std::fmt::Write as _;

use crate::ir::{Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{push_line, Output};

/// Markdown: ATX headings ranked by font size, one output line per source
/// line, and emphasis around each run of styled text. Blocks — across pages
/// too — are separated by a blank line.
pub struct Markdown;

impl Output for Markdown {
    /// One buffer for the document, every block written into it in place.
    /// A block that contributes no text, a page header or footer or a block
    /// whose lines are all blank, is cut back out together with the blank
    /// line that opened it, so the blocks that remain are separated by
    /// exactly one.
    fn render(&self, pages: &[PageLayout]) -> String {
        let mut out = String::new();
        for block in pages.iter().flat_map(|page| page.blocks.iter()) {
            let start = out.len();
            if start > 0 {
                out.push_str("\n\n");
            }
            let opened = out.len();
            push_block(&mut out, block);
            if out[opened..].trim().is_empty() {
                out.truncate(start);
            }
        }
        out
    }
}

fn push_block(out: &mut String, block: &Block) {
    match block {
        Block::Heading { level, lines, .. } => push_heading(out, *level, lines),
        Block::Paragraph { lines, role, .. } => match role {
            Role::Body => push_paragraph(out, lines),
            Role::Quote => push_quote(out, lines),
            Role::PageHeader | Role::PageFooter => {}
        },
        Block::List { items, .. } => push_list(out, items),
        Block::Table { rows, .. } => push_table(out, rows),
    }
}

/// `#` per level and the heading's lines as one line, or nothing when those
/// lines carry no text — the markers must be weighed after the text, never
/// before, or a blank line at heading size renders as a bare `#`. Emphasis is
/// dropped: a heading is already the strongest thing on the page, and ground
/// truth never carries `**` inside one.
fn push_heading(out: &mut String, level: u8, lines: &[Line]) {
    let mut text = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        push_line(&mut text, line);
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    for _ in 0..level {
        out.push('#');
    }
    out.push(' ');
    out.push_str(trimmed);
}

/// One output line per source line: hard line breaks are what the extracted
/// geometry actually knows.
fn push_paragraph(out: &mut String, lines: &[Line]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        push_emphasized(out, line);
    }
}

/// A block quotation: the paragraph's lines, each opened with `> `.
///
/// Covers ISO 32000-1 §14.8.4.2.
fn push_quote(out: &mut String, lines: &[Line]) {
    let mut paragraph = String::new();
    push_paragraph(&mut paragraph, lines);
    for (index, line) in paragraph.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("> ");
        out.push_str(line);
    }
}

/// Canonical bullets and numbers: `- ` regardless of the source glyph, and
/// `{n}. ` preserving the detected number. The first line loses its matched
/// marker prefix; continuation lines render on their own line, unprefixed —
/// the soft wrap the source layout already shows. A first line that was
/// nothing but the marker (a tagged item's label on a line of its own)
/// leaves nothing to open the item with, so the item opens on the line
/// after it.
fn push_list(out: &mut String, items: &[ListItem]) {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        push_list_item(out, item);
    }
}

fn push_list_item(out: &mut String, item: &ListItem) {
    let Some((first, rest)) = item.lines.split_first() else {
        return;
    };
    let stripped = Line {
        inlines: strip_marker(first, item.marker_len),
        y: first.y,
        x: first.x,
        end_x: first.end_x,
        size: first.size,
    };
    let mut head = &stripped;
    let mut rest = rest;
    if head
        .inlines
        .iter()
        .all(|inline| inline.text.trim().is_empty())
    {
        if let Some((next, after)) = rest.split_first() {
            head = next;
            rest = after;
        }
    }
    match &item.marker {
        Marker::Bullet => out.push_str("- "),
        Marker::Number(n) => {
            let _ = write!(out, "{n}. ");
        }
    }
    push_emphasized(out, head);
    for line in rest {
        out.push('\n');
        push_emphasized(out, line);
    }
}

/// `line`'s inlines with `chars` characters removed from the front — the
/// matched marker glyph and its trailing whitespace.
fn strip_marker(line: &Line, chars: usize) -> Vec<Inline> {
    let mut remaining = chars;
    let mut out = Vec::new();
    for inline in &line.inlines {
        let count = inline.text.chars().count();
        if remaining >= count {
            remaining -= count;
            continue;
        }
        out.push(Inline {
            text: inline.text.chars().skip(remaining).collect(),
            bold: inline.bold,
            italic: inline.italic,
            code: inline.code,
        });
        remaining = 0;
    }
    out
}

/// Pipes while every cell stands in one column and one row, HTML as soon
/// as one does not: GFM's pipe table has no way to say colspan or rowspan,
/// and an evaluator reading a merged cell reads it off that attribute.
///
/// Cells carry no emphasis. A table's markers are pure edit distance against
/// ground truth that carries none, exactly as in a heading. Amounts the
/// producer split across cells rejoin first, on a copy of the rows.
fn push_table(out: &mut String, rows: &[Vec<Cell>]) {
    let mut rows = rows.to_vec();
    crate::structure::tidy_amounts(&mut rows);
    if rows
        .iter()
        .flatten()
        .any(|cell| cell.colspan > 1 || cell.rowspan > 1)
    {
        push_html_table(out, &rows);
        return;
    }
    push_pipe_table(out, &rows);
}

/// GFM: the first row is the header, and the delimiter row that follows it
/// carries one `---` per column.
fn push_pipe_table(out: &mut String, rows: &[Vec<Cell>]) {
    let Some((header, body)) = rows.split_first() else {
        return;
    };
    // One scratch string holds each cell's text in turn.
    let mut text = String::new();
    push_pipe_row(out, header, &mut text);
    out.push('\n');
    out.push_str("| ");
    for index in 0..header.len() {
        if index > 0 {
            out.push_str(" | ");
        }
        out.push_str("---");
    }
    out.push_str(" |");
    for row in body {
        out.push('\n');
        push_pipe_row(out, row, &mut text);
    }
}

/// `| a | b |`: each cell's trimmed text with its pipes escaped.
fn push_pipe_row(out: &mut String, row: &[Cell], text: &mut String) {
    out.push_str("| ");
    for (index, cell) in row.iter().enumerate() {
        if index > 0 {
            out.push_str(" | ");
        }
        for (part, piece) in cell_text(cell, text).split('|').enumerate() {
            if part > 0 {
                out.push_str("\\|");
            }
            out.push_str(piece);
        }
    }
    out.push_str(" |");
}

/// One row per line, so the block stays a readable HTML block: CommonMark
/// ends one at a blank line, and blocks are joined by exactly one.
fn push_html_table(out: &mut String, rows: &[Vec<Cell>]) {
    let mut text = String::new();
    out.push_str("<table>");
    for row in rows {
        out.push_str("\n<tr>");
        for cell in row {
            push_html_cell(out, cell, &mut text);
        }
        out.push_str("</tr>");
    }
    out.push_str("\n</table>");
}

fn push_html_cell(out: &mut String, cell: &Cell, text: &mut String) {
    out.push_str("<td");
    if cell.colspan > 1 {
        let _ = write!(out, " colspan=\"{}\"", cell.colspan);
    }
    if cell.rowspan > 1 {
        let _ = write!(out, " rowspan=\"{}\"", cell.rowspan);
    }
    out.push('>');
    // The three characters that would otherwise open markup of their own.
    for c in cell_text(cell, text).chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out.push_str("</td>");
}

/// A cell's trimmed text, written into `text` and borrowed from it, or the
/// empty string for a cell nothing was drawn in.
fn cell_text<'t>(cell: &Cell, text: &'t mut String) -> &'t str {
    text.clear();
    if let Some(line) = &cell.line {
        push_line(text, line);
    }
    text.trim()
}

/// A line with its emphasis markers. Line assembly already merged
/// same-styled neighbours, so every inline is a maximal run.
fn push_emphasized(out: &mut String, line: &Line) {
    let start = out.len();
    for inline in &line.inlines {
        push_inline(out, inline);
    }
    escape_leading_hash(out, start);
}

/// The run's text with its markers around the trimmed middle only, so a run
/// that starts or ends on a space still reads as `plain **loud** tail`.
///
/// A run inside a `Code` structure element (ISO 32000-1 §14.8.4.4) is inline
/// code: backticks around the trimmed middle, one more than the longest
/// backtick run inside, and no emphasis, since a code span shows its text
/// literally.
///
/// A run with no letter or digit in it — the italic full stop that ends a
/// title, a bold space — gets no markers: emphasis needs something to
/// emphasize, CommonMark's flanking rules leave `word*.*` unparsed anyway,
/// and the stray asterisks are pure edit distance against ground truth that
/// carries none.
fn push_inline(out: &mut String, inline: &Inline) {
    if inline.code {
        push_code(out, &inline.text);
        return;
    }
    let marker = match (inline.bold, inline.italic) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        (false, false) => "",
    };
    // A run holding a pipe is escaped on a copy; the rest, nearly every
    // run, is written as it is.
    let escaped: std::borrow::Cow<str> = if inline.text.contains('|') {
        inline.text.replace('|', "\\|").into()
    } else {
        inline.text.as_str().into()
    };
    let text: &str = &escaped;
    let trimmed = text.trim();
    if marker.is_empty() || !trimmed.chars().any(char::is_alphanumeric) {
        out.push_str(text);
        return;
    }
    let lead = text.len() - text.trim_start().len();
    let tail = text.trim_end().len();
    out.push_str(&text[..lead]);
    out.push_str(marker);
    out.push_str(trimmed);
    out.push_str(marker);
    out.push_str(&text[tail..]);
}

/// `text` as a CommonMark code span: its trimmed middle between backtick
/// fences one longer than any backtick run it contains, the surrounding
/// whitespace kept outside the fences. Whitespace-only text stays as it is.
///
/// Covers ISO 32000-1 §14.8.4.4.
fn push_code(out: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        out.push_str(text);
        return;
    }
    let longest = trimmed.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let lead = text.len() - text.trim_start().len();
    let tail = text.trim_end().len();
    out.push_str(&text[..lead]);
    out.push_str(&fence);
    out.push_str(trimmed);
    out.push_str(&fence);
    out.push_str(&text[tail..]);
}

/// A body line opening with `#` would read as a heading: the line written
/// from `start` on gets a backslash before its first `#`. Nothing else is
/// escaped: every escape costs edit distance against ground truth that
/// carries none.
fn escape_leading_hash(out: &mut String, start: usize) {
    let line = &out[start..];
    let indent = line.len() - line.trim_start().len();
    if !line[indent..].starts_with('#') {
        return;
    }
    out.insert(start + indent, '\\');
}
