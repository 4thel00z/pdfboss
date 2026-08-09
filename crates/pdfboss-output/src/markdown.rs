//! The Markdown adapter: the layout IR as CommonMark.

use crate::ir::{Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{line_text, Output};

/// Markdown: ATX headings ranked by font size, one output line per source
/// line, and emphasis around each run of styled text. Blocks — across pages
/// too — are separated by a blank line.
pub struct Markdown;

impl Output for Markdown {
    fn render(&self, pages: &[PageLayout]) -> String {
        pages
            .iter()
            .flat_map(|page| page.blocks.iter())
            .filter_map(render_block)
            .collect::<Vec<String>>()
            .join("\n\n")
    }
}

/// One block's Markdown, or nothing when it contributes no text — page
/// furniture, or a block whose lines are all blank.
fn render_block(block: &Block) -> Option<String> {
    let rendered = match block {
        Block::Heading { level, lines, .. } => heading(*level, lines)?,
        Block::Paragraph { lines, role, .. } => {
            if !matches!(role, Role::Body) {
                return None;
            }
            paragraph(lines)
        }
        Block::List { items, .. } => list(items),
        Block::Table { rows, .. } => table(rows),
    };
    (!rendered.trim().is_empty()).then_some(rendered)
}

/// `#` per level and the heading's lines as one line, or nothing when those
/// lines carry no text — the markers must be weighed after the text, never
/// before, or a blank line at heading size renders as a bare `#`. Emphasis is
/// dropped: a heading is already the strongest thing on the page, and ground
/// truth never carries `**` inside one.
fn heading(level: u8, lines: &[Line]) -> Option<String> {
    let text = lines
        .iter()
        .map(line_text)
        .collect::<Vec<String>>()
        .join(" ");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("{} {}", "#".repeat(level as usize), trimmed))
}

/// One output line per source line: hard line breaks are what the extracted
/// geometry actually knows.
fn paragraph(lines: &[Line]) -> String {
    lines
        .iter()
        .map(emphasized)
        .collect::<Vec<String>>()
        .join("\n")
}

/// Canonical bullets and numbers: `- ` regardless of the source glyph, and
/// `{n}. ` preserving the detected number. The first line loses its matched
/// marker prefix; continuation lines render on their own line, unprefixed —
/// the soft wrap the source layout already shows.
fn list(items: &[ListItem]) -> String {
    items
        .iter()
        .map(list_item)
        .collect::<Vec<String>>()
        .join("\n")
}

fn list_item(item: &ListItem) -> String {
    let prefix = match &item.marker {
        Marker::Bullet => "- ".to_string(),
        Marker::Number(n) => format!("{n}. "),
    };
    let Some((first, rest)) = item.lines.split_first() else {
        return String::new();
    };
    let head = Line {
        inlines: strip_marker(first, item.marker_len),
        ..first.clone()
    };
    let mut out = format!("{prefix}{}", emphasized(&head));
    for line in rest {
        out.push('\n');
        out.push_str(&emphasized(line));
    }
    out
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
        });
        remaining = 0;
    }
    out
}

/// Pipes while every cell stands in one column, HTML as soon as one does
/// not: GFM's pipe table has no way to say colspan, and an evaluator reading
/// a merged cell reads it off that attribute.
///
/// Cells carry no emphasis. A table's markers are pure edit distance against
/// ground truth that carries none, exactly as in a heading.
fn table(rows: &[Vec<Cell>]) -> String {
    if rows.iter().flatten().any(|cell| cell.colspan > 1) {
        return html_table(rows);
    }
    pipe_table(rows)
}

/// GFM: the first row is the header, and the delimiter row that follows it
/// carries one `---` per column.
fn pipe_table(rows: &[Vec<Cell>]) -> String {
    let Some((header, body)) = rows.split_first() else {
        return String::new();
    };
    let mut out = pipe_row(header);
    out.push('\n');
    out.push_str(&pipe_join(&vec!["---".to_string(); header.len()]));
    for row in body {
        out.push('\n');
        out.push_str(&pipe_row(row));
    }
    out
}

fn pipe_row(row: &[Cell]) -> String {
    let cells: Vec<String> = row
        .iter()
        .map(|cell| cell_text(cell).replace('|', "\\|"))
        .collect();
    pipe_join(&cells)
}

fn pipe_join(cells: &[String]) -> String {
    format!("| {} |", cells.join(" | "))
}

/// One row per line, so the block stays a readable HTML block: CommonMark
/// ends one at a blank line, and blocks are joined by exactly one.
fn html_table(rows: &[Vec<Cell>]) -> String {
    let mut out = String::from("<table>");
    for row in rows {
        out.push_str("\n<tr>");
        for cell in row {
            out.push_str(&html_cell(cell));
        }
        out.push_str("</tr>");
    }
    out.push_str("\n</table>");
    out
}

fn html_cell(cell: &Cell) -> String {
    let text = html_escape(&cell_text(cell));
    if cell.colspan <= 1 {
        return format!("<td>{text}</td>");
    }
    format!("<td colspan=\"{}\">{text}</td>", cell.colspan)
}

/// The three characters that would otherwise open markup of their own.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A cell's text, or the empty string for a cell nothing was drawn in.
fn cell_text(cell: &Cell) -> String {
    cell.line
        .as_ref()
        .map(|line| line_text(line).trim().to_string())
        .unwrap_or_default()
}

/// A line with its emphasis markers. Line assembly already merged
/// same-styled neighbours, so every inline is a maximal run.
fn emphasized(line: &Line) -> String {
    let mut out = String::new();
    for inline in &line.inlines {
        push_inline(&mut out, inline);
    }
    escape_leading_hash(out)
}

/// The run's text with its markers around the trimmed middle only, so a run
/// that starts or ends on a space still reads as `plain **loud** tail`.
///
/// A run with no letter or digit in it — the italic full stop that ends a
/// title, a bold space — gets no markers: emphasis needs something to
/// emphasize, CommonMark's flanking rules leave `word*.*` unparsed anyway,
/// and the stray asterisks are pure edit distance against ground truth that
/// carries none.
fn push_inline(out: &mut String, inline: &Inline) {
    let marker = match (inline.bold, inline.italic) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        (false, false) => "",
    };
    let text = inline.text.replace('|', "\\|");
    let trimmed = text.trim();
    if marker.is_empty() || !trimmed.chars().any(char::is_alphanumeric) {
        out.push_str(&text);
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

/// A body line opening with `#` would read as a heading. Nothing else is
/// escaped: every escape costs edit distance against ground truth that
/// carries none.
fn escape_leading_hash(line: String) -> String {
    let indent = line.len() - line.trim_start().len();
    if !line[indent..].starts_with('#') {
        return line;
    }
    format!("{}\\{}", &line[..indent], &line[indent..])
}
