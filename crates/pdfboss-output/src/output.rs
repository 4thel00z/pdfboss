//! Output adapters: the renderers that turn the layout IR into a document.

use crate::ir::{Block, Line, PageLayout};

/// Renders laid-out pages into one document.
pub trait Output {
    fn render(&self, pages: &[PageLayout]) -> String;
}

/// Plain text: every line of every block in reading order joined with `\n`,
/// pages separated by a form feed. Structure is invisible here — this is the
/// adapter that must stay byte-equal to positional text extraction.
pub struct Text;

impl Output for Text {
    fn render(&self, pages: &[PageLayout]) -> String {
        let mut out = String::new();
        for (page_index, page) in pages.iter().enumerate() {
            if page_index > 0 {
                out.push('\u{c}');
            }
            let mut written = 0usize;
            for block in &page.blocks {
                match block {
                    Block::Heading { lines, .. } | Block::Paragraph { lines, .. } => {
                        for line in lines {
                            open_line(&mut out, &mut written);
                            push_line(&mut out, line);
                        }
                    }
                    Block::List { items, .. } => {
                        for line in items.iter().flat_map(|item| &item.lines) {
                            open_line(&mut out, &mut written);
                            push_line(&mut out, line);
                        }
                    }
                    Block::Table { rows, .. } => {
                        for row in rows {
                            open_line(&mut out, &mut written);
                            push_row(&mut out, row);
                        }
                    }
                }
            }
        }
        out
    }
}

/// Opens an output line: every line but the page's first is preceded by
/// `\n`.
fn open_line(out: &mut String, written: &mut usize) {
    if *written > 0 {
        out.push('\n');
    }
    *written += 1;
}

/// A line as unmarked text: what every adapter starts from.
pub(crate) fn line_text(line: &Line) -> String {
    let mut out = String::new();
    push_line(&mut out, line);
    out
}

/// A line's inline runs concatenated; the runs already carry the spaces the
/// word-gap rule inserted.
fn push_line(out: &mut String, line: &Line) {
    for inline in &line.inlines {
        out.push_str(&inline.text);
    }
}

/// A table row reads as one visual line: its cells' text separated by the
/// single space a qualifying word gap would have left. Cells with no line
/// contribute nothing.
fn push_row(out: &mut String, row: &[crate::ir::Cell]) {
    for (index, line) in row.iter().filter_map(|cell| cell.line.as_ref()).enumerate() {
        if index > 0 {
            out.push(' ');
        }
        push_line(out, line);
    }
}
