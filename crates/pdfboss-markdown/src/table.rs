//! Table layout: content-sized columns, proportional fit, and per-row
//! placement that never splits a row across a page break.

use pdfboss_style::{Align, Edges, Element, TextStyle, Theme};
use pdfboss_write::Color;

use crate::block::{CellAlign, Run};
use crate::layout::{frag_items, styled_runs, Engine, Item, BASELINE};
use crate::report::Report;
use crate::wrap::{wrap, LineBox};
use crate::Error;

/// Table cell border stroke width, in points.
const BORDER_WIDTH: f32 = 0.5;

/// Table cell border color.
const BORDER_COLOR: Color = Color::Gray(0.6);

/// Lays out a table's header and body rows, sizing columns to their
/// content and scaling proportionally to fit `right - left`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_table(
    engine: &mut Engine,
    aligns: &[CellAlign],
    head: &[Vec<Run>],
    rows: &[Vec<Vec<Run>>],
    base: &TextStyle,
    left: f32,
    right: f32,
    report: &mut Report,
) -> Result<(), Error> {
    let theme = engine.theme;
    let th = base.apply(theme.declared(Element::Th));
    let td = base.apply(theme.declared(Element::Td));
    let th_padding = theme.padding(Element::Th);
    let td_padding = theme.padding(Element::Td);
    let th_background = theme.background(Element::Th);
    let td_background = theme.background(Element::Td);
    let margin = theme.margin(Element::Table);

    let columns = aligns
        .len()
        .max(head.len())
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![0.0f32; columns];
    for (j, width) in widths.iter_mut().enumerate() {
        *width = natural(theme, head.get(j), &th, &th_padding)?;
    }
    for row in rows {
        for (j, width) in widths.iter_mut().enumerate() {
            *width = width.max(natural(theme, row.get(j), &td, &td_padding)?);
        }
    }
    fit(&mut widths, right - left);

    engine.gap(margin.top);
    place_row(
        engine,
        head,
        aligns,
        &widths,
        &th,
        &th_padding,
        th_background,
        left,
        report,
    )?;
    for row in rows {
        place_row(
            engine,
            row,
            aligns,
            &widths,
            &td,
            &td_padding,
            td_background,
            left,
            report,
        )?;
    }
    engine.after(margin.bottom);
    Ok(())
}

/// A cell's natural single-line width: its styled runs wrapped at
/// `f32::MAX` (so nothing wraps), widest resulting line, plus the cell's
/// own left and right padding. A missing cell (a ragged row shorter than
/// the header) counts as `0` beyond its padding.
///
/// Sanitizes into a throwaway report: the row-placement pass sanitizes the
/// same runs again and is the one that should tally replacements, so this
/// sizing pass must not double-count them.
fn natural(
    theme: &Theme,
    runs: Option<&Vec<Run>>,
    style: &TextStyle,
    padding: &Edges,
) -> Result<f32, Error> {
    let empty = Vec::new();
    let mut discard = Report::default();
    let styled = styled_runs(theme, runs.unwrap_or(&empty), style, &mut discard);
    let lines = wrap(&styled, f32::MAX, style.size)?;
    let width = lines.iter().map(|line| line.width).fold(0.0, f32::max);
    Ok(width + padding.left + padding.right)
}

/// Scales `widths` down proportionally so they sum to at most `available`,
/// leaving them untouched when they already fit.
fn fit(widths: &mut [f32], available: f32) {
    let total: f32 = widths.iter().sum();
    if total <= available {
        return;
    }
    let scale = available / total;
    for width in widths.iter_mut() {
        *width *= scale;
    }
}

/// Places one row's cells left to right at the current write position:
/// each cell's background then border then manually placed lines, the
/// whole row requested via [`Engine::need`] so it never splits across a
/// page break.
#[allow(clippy::too_many_arguments)]
fn place_row(
    engine: &mut Engine,
    cells: &[Vec<Run>],
    aligns: &[CellAlign],
    widths: &[f32],
    style: &TextStyle,
    padding: &Edges,
    background: Option<Color>,
    left: f32,
    report: &mut Report,
) -> Result<(), Error> {
    let theme = engine.theme;
    let empty = Vec::new();
    let mut wrapped: Vec<Vec<LineBox>> = Vec::with_capacity(widths.len());
    for (j, width) in widths.iter().enumerate() {
        let runs = cells.get(j).unwrap_or(&empty);
        let styled = styled_runs(theme, runs, style, report);
        let inner = (width - padding.left - padding.right).max(0.0);
        wrapped.push(wrap(&styled, inner, style.size)?);
    }
    let content_h = wrapped
        .iter()
        .map(|lines| {
            lines
                .iter()
                .map(|line| style.line_height * line.max_size)
                .sum::<f32>()
        })
        .fold(0.0, f32::max);
    let row_h = content_h + padding.top + padding.bottom;

    engine.need(row_h);
    let top = engine.y;
    let mut x = left;
    for (j, lines) in wrapped.into_iter().enumerate() {
        let width = widths[j];
        if let Some(color) = background {
            engine.push(Item::Rect {
                x,
                y: top - row_h,
                w: width,
                h: row_h,
                color,
            });
        }
        engine.push(Item::Frame {
            x,
            y: top - row_h,
            w: width,
            h: row_h,
            width: BORDER_WIDTH,
            color: BORDER_COLOR,
        });
        let align = effective_align(
            aligns.get(j).copied().unwrap_or(CellAlign::Default),
            style.align,
        );
        let mut cursor = top - padding.top;
        for line in &lines {
            let h = style.line_height * line.max_size;
            let baseline = cursor - BASELINE * line.max_size;
            let origin = match align {
                Align::Left => x + padding.left,
                Align::Center => {
                    x + padding.left + (width - padding.left - padding.right - line.width) / 2.0
                }
                Align::Right => x + width - padding.right - line.width,
            };
            for item in frag_items(&line.frags, origin, baseline) {
                engine.push(item);
            }
            cursor -= h;
        }
        x += width;
    }
    engine.y -= row_h;
    engine.at_top = false;
    Ok(())
}

/// The alignment a cell actually renders with: the pipe alignment from the
/// header separator when given, else the cell style's own text alignment.
fn effective_align(pipe: CellAlign, style_align: Align) -> Align {
    match pipe {
        CellAlign::Default => style_align,
        CellAlign::Left => Align::Left,
        CellAlign::Center => Align::Center,
        CellAlign::Right => Align::Right,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pdfboss_style::Theme;
    use pdfboss_write::{PageSize, Standard14};

    use super::*;
    use crate::block::parse_blocks;
    use crate::layout::{layout, Item, LaidPage};

    fn laid(md: &str, css: &str) -> Vec<LaidPage> {
        laid_with_report(md, css).0
    }

    fn laid_with_report(md: &str, css: &str) -> (Vec<LaidPage>, Report) {
        let theme = Theme::parse(css).unwrap();
        let (blocks, _) = parse_blocks(md);
        let mut report = Report::default();
        let pages = layout(&blocks, &theme, PageSize::A4, Path::new("."), &mut report).unwrap();
        (pages, report)
    }

    const MONO: &str =
        "body { font-family: courier; font-size: 10pt; line-height: 1.0; margin: 100pt; }";

    #[test]
    fn columns_size_to_content_and_scale_to_fit() {
        let pages = laid("| a | bbbb |\n|---|---|\n| c | d |\n", MONO);
        let frames: Vec<(f32, f32)> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Frame { x, w, .. } => Some((*x, *w)),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 4);
        let narrow = frames.iter().map(|(_, w)| *w).fold(f32::MAX, f32::min);
        let wide = frames.iter().map(|(_, w)| *w).fold(0.0, f32::max);
        assert!(wide > narrow, "content-sized columns: {frames:?}");
    }

    #[test]
    fn header_cells_are_bold_with_background() {
        let pages = laid("| h |\n|---|\n| b |\n", MONO);
        assert!(
            pages[0]
                .items
                .iter()
                .any(|i| matches!(i, Item::Rect { .. })),
            "th background"
        );
        assert!(pages[0].items.iter().any(
            |i| matches!(i, Item::Text { text, font, .. } if text == "h" && *font == Standard14::CourierBold)
        ));
    }

    #[test]
    fn column_alignment_follows_the_pipes() {
        let pages = laid("| L | R |\n|:--|--:|\n| aa | bb |\n", MONO);
        let texts: Vec<(String, f32)> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, x, .. } => Some((text.clone(), *x)),
                _ => None,
            })
            .collect();
        let (_, left_x) = texts.iter().find(|(t, _)| t == "aa").unwrap();
        let (_, right_x) = texts.iter().find(|(t, _)| t == "bb").unwrap();
        assert!(*right_x > *left_x + 6.0, "right-aligned cell shifts right");
    }

    #[test]
    fn rows_break_between_not_inside() {
        let mut md = String::from("| h |\n|---|\n");
        for i in 0..120 {
            md.push_str(&format!("| row{i} |\n"));
        }
        let pages = laid(&md, MONO);
        assert!(pages.len() >= 2);
    }

    #[test]
    fn links_and_decorations_place_inside_cells() {
        let pages = laid("| [d](https://x.y) |\n|---|\n| b |\n", MONO);
        assert!(
            pages[0]
                .items
                .iter()
                .any(|i| matches!(i, Item::Link { uri, .. } if uri == "https://x.y")),
            "cell link rect"
        );
        assert!(
            pages[0]
                .items
                .iter()
                .any(|i| matches!(i, Item::Stroke { .. })),
            "cell link underline via the shared frag_items helper"
        );
    }

    #[test]
    fn unencodable_cell_characters_are_reported_once() {
        let (_, report) = laid_with_report("| \u{4e2d} |\n|---|\n| b |\n", MONO);
        assert_eq!(
            report.replaced.get(&'\u{4e2d}'),
            Some(&1),
            "the column-sizing pass must not double-count sanitize replacements: {report:?}"
        );
    }
}
