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
/// single space a qualifying word gap would have left, and by nothing more
/// when a cell already ends or begins with one. Cells with no line
/// contribute nothing.
fn push_row(out: &mut String, row: &[crate::ir::Cell]) {
    let row_start = out.len();
    for line in row.iter().filter_map(|cell| cell.line.as_ref()) {
        let text = line_text(line);
        let separated = out.len() == row_start
            || out.ends_with(char::is_whitespace)
            || text.starts_with(char::is_whitespace);
        if !separated {
            out.push(' ');
        }
        out.push_str(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BBox, Cell, Inline};

    fn cell(text: &str) -> Cell {
        Cell {
            line: Some(Line {
                inlines: vec![Inline {
                    text: text.to_string(),
                    bold: false,
                    italic: false,
                }],
                y: 700.0,
                x: 72.0,
                end_x: 100.0,
                size: 10.0,
            }),
            colspan: 1,
            rowspan: 1,
        }
    }

    /// A cell whose text ends in a space glyph, or whose neighbour starts
    /// with one, is not separated from it by a second space.
    #[test]
    fn a_table_row_joins_cells_with_one_space() {
        let page = PageLayout {
            blocks: vec![Block::Table {
                rows: vec![vec![cell("0.933 "), cell("0.215"), cell(" 0.216")]],
                bbox: BBox {
                    x0: 72.0,
                    y0: 700.0,
                    x1: 300.0,
                    y1: 710.0,
                },
            }],
        };
        assert_eq!(Text.render(&[page]), "0.933 0.215 0.216");
    }
}
