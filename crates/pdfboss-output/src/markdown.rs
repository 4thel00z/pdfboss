//! The Markdown adapter: the layout IR as CommonMark.

use crate::ir::{Block, Cell, Inline, Line, ListItem, PageLayout, Role};
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
        Block::Heading { level, lines, .. } => heading(*level, lines),
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

/// `#` per level and the heading's lines as one line. Emphasis is dropped:
/// a heading is already the strongest thing on the page, and ground truth
/// never carries `**` inside one.
fn heading(level: u8, lines: &[Line]) -> String {
    let text = lines
        .iter()
        .map(line_text)
        .collect::<Vec<String>>()
        .join(" ");
    format!("{} {}", "#".repeat(level as usize), text.trim())
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

/// Task 6 gives lists their markers; until then a list reads as its raw
/// lines, exactly as plain text does.
fn list(items: &[ListItem]) -> String {
    items
        .iter()
        .flat_map(|item| item.lines.iter())
        .map(emphasized)
        .collect::<Vec<String>>()
        .join("\n")
}

/// Task 7 gives tables pipes; until then a row reads as one line, exactly
/// as plain text does.
fn table(rows: &[Vec<Cell>]) -> String {
    rows.iter()
        .map(|row| row_text(row))
        .collect::<Vec<String>>()
        .join("\n")
}

fn row_text(row: &[Cell]) -> String {
    row.iter()
        .filter_map(|cell| cell.line.as_ref())
        .map(emphasized)
        .collect::<Vec<String>>()
        .join(" ")
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
