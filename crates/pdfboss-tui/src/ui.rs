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
    use pdfboss_core::{Dict, Name, ObjRef, Object};
    use pdfboss_render::Pixmap;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use ratatui::Terminal;

    #[test]
    fn panes_split_80_by_24_deterministically() {
        let split = panes(Rect::new(0, 0, 80, 24));
        assert_eq!(split.tree, Rect::new(0, 0, 28, 23));
        assert_eq!(split.right_top, Rect::new(28, 0, 52, 14));
        assert_eq!(split.hex, Rect::new(28, 14, 52, 9));
        assert_eq!(split.status, Rect::new(0, 23, 80, 1));
    }

    fn test_split() -> Panes {
        panes(Rect::new(0, 0, 80, 24))
    }

    fn draw_frame(app: &App) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        terminal.draw(|frame| draw(app, frame)).expect("draw");
        terminal
    }

    /// Concatenates cell symbols across one row, for substring checks on a
    /// pane's border/title line.
    fn row_text(buffer: &Buffer, y: u16, x0: u16, width: u16) -> String {
        (x0..x0 + width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    fn two_by_four_pixmap() -> Pixmap {
        Pixmap {
            width: 2,
            height: 4,
            data: vec![
                255, 0, 0, 255, 0, 255, 0, 255, // row 0: red, green
                0, 0, 255, 255, 255, 255, 0, 255, // row 1: blue, yellow
                255, 0, 255, 255, 0, 255, 255, 255, // row 2: magenta, cyan
                128, 128, 128, 255, 0, 0, 0, 255, // row 3: gray, black
            ],
        }
    }

    /// Regression guard for `pane_block`: the focused pane's title carries
    /// `BOLD`, and the same title carries no `BOLD` once focus moves away.
    #[test]
    fn focused_pane_title_is_bold_others_are_not() {
        let split = test_split();
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        assert_eq!(app.focus, Pane::Tree, "default focus starts on Tree");

        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();
        assert!(
            buffer[(split.tree.x + 1, split.tree.y)]
                .modifier
                .contains(Modifier::BOLD),
            "focused Tree title is bold"
        );
        assert!(
            !buffer[(split.right_top.x + 1, split.right_top.y)]
                .modifier
                .contains(Modifier::BOLD),
            "unfocused Inspector title is not bold"
        );

        app.focus = Pane::Inspector;
        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();
        assert!(
            !buffer[(split.tree.x + 1, split.tree.y)]
                .modifier
                .contains(Modifier::BOLD),
            "Tree lost focus, its title is no longer bold"
        );
        assert!(
            buffer[(split.right_top.x + 1, split.right_top.y)]
                .modifier
                .contains(Modifier::BOLD),
            "Inspector gained focus, its title is bold"
        );
    }

    /// Regression guard for `draw_tree`: the selected row's cells carry
    /// `REVERSED`; every other visible row does not.
    #[test]
    fn selected_tree_row_is_reversed() {
        let split = test_split();
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        app.tree.selected = app.tree.pages_folder;
        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();
        let x = split.tree.x + 1;
        let document_row = split.tree.y + 1; // row 0: Document (not selected)
        let pages_row = split.tree.y + 2; // row 1: Pages (selected)
        assert!(
            !buffer[(x, document_row)]
                .modifier
                .contains(Modifier::REVERSED),
            "unselected Document row is not reversed"
        );
        assert!(
            buffer[(x, pages_row)].modifier.contains(Modifier::REVERSED),
            "selected Pages row is reversed"
        );
    }

    /// Regression guard for `draw_inspector`: the line under the ref cursor
    /// carries `REVERSED` while the inspector is focused; other lines do not.
    #[test]
    fn inspector_cursor_line_is_reversed_when_focused() {
        let split = test_split();
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        let mut dict = Dict::new();
        dict.insert(
            Name("Ref".to_string()),
            Object::Ref(ObjRef { num: 2, gen: 0 }),
        );
        app.inspector
            .set_object(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
        app.inspector.move_cursor(1);
        app.focus = Pane::Inspector;
        let cursor_line = app
            .inspector
            .ref_cursor
            .and_then(|index| app.inspector.refs.get(index))
            .map(|(line, ..)| *line)
            .expect("cursor moved onto the one ref");
        assert!(
            app.inspector.lines.len() > 1,
            "need a non-cursor line to contrast against"
        );
        let other_line = if cursor_line == 0 {
            app.inspector.lines.len() - 1
        } else {
            0
        };

        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();
        let x = split.right_top.x + 1;
        let cursor_y = split.right_top.y + 1 + cursor_line as u16;
        let other_y = split.right_top.y + 1 + other_line as u16;
        assert!(
            buffer[(x, cursor_y)].modifier.contains(Modifier::REVERSED),
            "ref-cursor line is reversed"
        );
        assert!(
            !buffer[(x, other_y)].modifier.contains(Modifier::REVERSED),
            "non-cursor line is not reversed"
        );
    }

    /// Regression guard for `draw_preview`: an active preview with a ready
    /// pixmap replaces the inspector, paints half-block (`▀`) cells whose
    /// fg/bg match `cell_colors`, and only as many rows as
    /// `div_ceil(height, 2)`.
    #[test]
    fn preview_active_pixmap_replaces_inspector_with_half_blocks() {
        let split = test_split();
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        let pixmap = two_by_four_pixmap();
        let expected_rows = pixmap.height.div_ceil(2);
        app.preview.active = true;
        app.preview.pixmap = Some(pixmap);

        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();

        let title = row_text(
            buffer,
            split.right_top.y,
            split.right_top.x,
            split.right_top.width,
        );
        assert!(title.contains("Preview"), "preview title shown: {title}");
        assert!(!title.contains("Inspector"), "inspector replaced: {title}");

        let x0 = split.right_top.x + 1;
        let y0 = split.right_top.y + 1;
        assert_eq!(
            expected_rows, 2,
            "4 pixel rows pack into 2 half-block cell rows"
        );

        let cell = &buffer[(x0, y0)];
        assert_eq!(cell.symbol(), "\u{2580}", "half-block glyph");
        assert_eq!(cell.fg, Color::Rgb(255, 0, 0));
        assert_eq!(cell.bg, Color::Rgb(0, 0, 255));

        let cell = &buffer[(x0 + 1, y0)];
        assert_eq!(cell.fg, Color::Rgb(0, 255, 0));
        assert_eq!(cell.bg, Color::Rgb(255, 255, 0));

        let cell = &buffer[(x0, y0 + 1)];
        assert_eq!(cell.fg, Color::Rgb(255, 0, 255));
        assert_eq!(cell.bg, Color::Rgb(128, 128, 128));

        // Only `expected_rows` (2) half-block rows exist; the row after
        // must be blank, not another painted pixel row.
        assert_ne!(
            buffer[(x0, y0 + expected_rows as u16)].symbol(),
            "\u{2580}",
            "no third half-block row"
        );
    }

    /// Regression guard for `draw_preview`'s loading branch: while
    /// rendering (no pixmap installed yet) the spinner char leads the line.
    #[test]
    fn preview_rendering_without_pixmap_shows_spinner() {
        let split = test_split();
        let mut app = App::new("t.pdf".to_string(), (1, 7), 1, (80, 24));
        app.preview.active = true;
        app.preview.rendering = true;
        app.preview.spinner_frame = 2; // SPINNER[2] == '-'

        let terminal = draw_frame(&app);
        let buffer = terminal.backend().buffer();
        let line = row_text(
            buffer,
            split.right_top.y + 1,
            split.right_top.x + 1,
            split.right_top.width - 2,
        );
        assert!(line.starts_with('-'), "spinner char leads the line: {line}");
        assert!(line.contains("rendering"), "rendering label shown: {line}");
    }
}
