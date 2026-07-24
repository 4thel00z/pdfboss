//! Rendering: pure layout math here plus the pane painters.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{App, Pane};
use crate::hexview::{hex_line, highlight_cols, BYTES_PER_LINE};
use crate::inspector::InspectorMode;
use crate::preview::{cell_colors, SPINNER};

/// The four screen regions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Panes {
    pub tree: Rect,
    /// Inspector, or the page preview while it is active.
    pub right_top: Rect,
    pub hex: Rect,
    pub status: Rect,
}

/// Splits the terminal: status bar (1 row) at the bottom; tree pane at
/// ~35% width; right column split 60/40 into inspector and hex.
pub fn panes(area: Rect) -> Panes {
    let rows = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let columns =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).split(rows[0]);
    let right = Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(columns[1]);
    Panes {
        tree: columns[0],
        right_top: right[0],
        hex: right[1],
        status: rows[1],
    }
}

/// Renders the whole app into one frame.
pub fn draw(app: &App, frame: &mut Frame) {
    let split = panes(frame.area());
    draw_tree(app, frame, split.tree);
    if app.preview.active {
        draw_preview(app, frame, split.right_top);
    } else {
        draw_inspector(app, frame, split.right_top);
    }
    draw_hex(app, frame, split.hex);
    draw_status(app, frame, split.status);
}

fn pane_block(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block.title_style(Style::default().add_modifier(Modifier::BOLD))
    } else {
        block
    }
}

fn draw_tree(app: &App, frame: &mut Frame, area: Rect) {
    let block = pane_block("Tree".to_string(), app.focus == Pane::Tree);
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows = app.tree.visible_rows();
    let selected_position = rows
        .iter()
        .position(|row| row.id == app.tree.selected)
        .unwrap_or(0);
    let offset = selected_position.saturating_sub(inner_height.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::new();
    for row in rows.iter().skip(offset).take(inner_height) {
        let glyph = if app.tree.is_branch(row.id) {
            if app.tree.node(row.id).expanded {
                "\u{25be} "
            } else {
                "\u{25b8} "
            }
        } else {
            "  "
        };
        let text = format!(
            "{}{}{}",
            "  ".repeat(row.depth),
            glyph,
            app.tree.label(row.id)
        );
        let style = if row.id == app.tree.selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(text, style));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_inspector(app: &App, frame: &mut Frame, area: Rect) {
    let mode_suffix = if app.inspector.is_stream() && app.inspector.mode != InspectorMode::Pretty {
        format!(" [{}]", app.inspector.mode_name())
    } else {
        String::new()
    };
    let title = if app.inspector.title.is_empty() {
        "Inspector".to_string()
    } else {
        format!("Inspector \u{b7} {}{}", app.inspector.title, mode_suffix)
    };
    let block = pane_block(title, app.focus == Pane::Inspector);
    let cursor_line = app
        .inspector
        .ref_cursor
        .and_then(|index| app.inspector.refs.get(index))
        .map(|(line, ..)| *line);
    let lines: Vec<Line> = app
        .inspector
        .lines
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let style = if Some(index) == cursor_line {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::styled(text.clone(), style)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.inspector.scroll, 0))
            .block(block),
        area,
    );
}

fn draw_hex(app: &App, frame: &mut Frame, area: Rect) {
    let block = pane_block(app.hex.title(), app.focus == Pane::Hex);
    let inner_height = u64::from(area.height.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(error) = &app.hex.error {
        lines.push(Line::raw(format!("error: {error}")));
    } else if app.hex.loading {
        lines.push(Line::raw("loading\u{2026}"));
    } else if app.hex.source.is_some() {
        let mut row = 0u64;
        while row < inner_height {
            let line_index = app.hex.scroll_line + row;
            if line_index >= app.hex.line_count() {
                break;
            }
            let offset = line_index * BYTES_PER_LINE as u64;
            let end = (offset + BYTES_PER_LINE as u64).min(app.hex.total_len);
            let window_end = app.hex.window_start + app.hex.bytes.len() as u64;
            if offset < app.hex.window_start || end > window_end {
                // Bytes outside the resident window (a fetch is in flight).
                lines.push(Line::raw("\u{2026}"));
            } else {
                let first = (offset - app.hex.window_start) as usize;
                let last = (end - app.hex.window_start) as usize;
                let slice = &app.hex.bytes[first..last];
                let hl = app
                    .hex
                    .highlight
                    .and_then(|span| highlight_cols(offset, slice.len(), span));
                lines.push(hex_line(app.hex.base + offset, slice, hl));
            }
            row += 1;
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_preview(app: &App, frame: &mut Frame, area: Rect) {
    let title = match app.preview.page {
        Some(page) => format!("Preview \u{b7} page {}", page + 1),
        None => "Preview".to_string(),
    };
    let block = pane_block(title, app.focus == Pane::Inspector);
    let inner_width = u32::from(area.width.saturating_sub(2));
    let inner_height = u32::from(area.height.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if app.preview.rendering {
        lines.push(Line::raw(format!(
            "{} rendering\u{2026}",
            SPINNER[app.preview.spinner_frame]
        )));
    } else if let Some(error) = &app.preview.error {
        lines.push(Line::raw(format!("error: {error}")));
    } else if let Some(pix) = &app.preview.pixmap {
        let columns = pix.width.min(inner_width);
        let rows = pix.height.div_ceil(2).min(inner_height);
        let mut row = 0u32;
        while row < rows {
            let mut cells: Vec<Span> = Vec::new();
            let mut column = 0u32;
            while column < columns {
                let (fg, bg) = cell_colors(pix, column, row);
                cells.push(Span::styled("\u{2580}", Style::default().fg(fg).bg(bg)));
                column += 1;
            }
            lines.push(Line::from(cells));
            row += 1;
        }
    } else {
        lines.push(Line::raw("no preview yet"));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new(app.status_line()), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_split_80_by_24_deterministically() {
        let split = panes(Rect::new(0, 0, 80, 24));
        assert_eq!(split.tree, Rect::new(0, 0, 28, 23));
        assert_eq!(split.right_top, Rect::new(28, 0, 52, 14));
        assert_eq!(split.hex, Rect::new(28, 14, 52, 9));
        assert_eq!(split.status, Rect::new(0, 23, 80, 1));
    }
}
