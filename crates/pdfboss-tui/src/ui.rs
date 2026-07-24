//! Rendering: pure layout math here plus (Task 9) the pane painters.

use ratatui::layout::{Constraint, Layout, Rect};

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
