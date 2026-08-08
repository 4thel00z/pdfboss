//! Spans to the layout IR: line assembly, word gaps, and the two-column
//! gutter split.

use crate::ir::{BBox, Block, Inline, Line, PageLayout, Role};
use crate::output::{Output, Text};
use pdfboss_text::TextSpan;

/// Fraction of the device font size a horizontal gap must exceed to read
/// as a word break. The ceiling is justified LaTeX's shrunk inter-word
/// glue — 0.17 em for Times-family fonts, and a hair less under a
/// compressed text matrix — and the floor is italic corrections and
/// kerns, which stay under 0.1 em; 0.25 em sat exactly on the nominal
/// Times space width and swallowed every shrunk line's spaces.
const WORD_GAP: f32 = 0.15;

/// Minimum column-candidate spans on a page before a gutter is looked for.
const COLUMN_MIN_SPANS: usize = 40;
/// Minimum spans and distinct baselines on each side of a candidate gutter.
const COLUMN_MIN_SIDE_SPANS: usize = 10;
const COLUMN_MIN_SIDE_LINES: usize = 6;
/// Each column must cover at least this fraction of the combined text
/// height — low enough that a final page whose right column ends early
/// still splits, high enough that a sidebar note does not.
const COLUMN_MIN_HEIGHT: f32 = 0.4;
/// Each column must also span at least this fraction of the text width:
/// a table's number or label column is far narrower than any genuine text
/// column, and splitting a table reads its rows column-major.
const COLUMN_MIN_SIDE_WIDTH: f32 = 0.25;
/// Minimum device-space gutter width, and the central band of the text
/// width the gutter's center must fall in.
const GUTTER_MIN_WIDTH: f32 = 6.0;
const GUTTER_BAND: std::ops::RangeInclusive<f32> = 0.25..=0.75;
/// A span wider than this fraction of the text width separates bands
/// (headings, footers) rather than belonging to either column.
const SEPARATOR_FRACTION: f32 = 0.5;
/// Occupancy-histogram resolution for gutter detection.
const GUTTER_BINS: usize = 128;

/// Groups spans into lines (baselines within `0.5 · size`), orders lines
/// top to bottom and spans left to right, inserts a space at horizontal
/// gaps wider than [`WORD_GAP`] times the size, and joins lines with `\n`.
/// A page with a clear two-column gutter reads column-major: full-width
/// separators split it into bands, and within each band the left column
/// flows before the right (see [`segments`]).
pub fn layout(spans: &[TextSpan]) -> String {
    Text.render(&[page_layout(spans)])
}

/// The page's spans as structure: one [`Block::Paragraph`] per reading-order
/// segment (see [`segments`]), each holding the segment's assembled lines.
pub fn page_layout(spans: &[TextSpan]) -> PageLayout {
    let mut blocks = Vec::new();
    for segment in segments(spans) {
        if segment.is_empty() {
            continue;
        }
        let lines = assemble_lines(&segment);
        let bbox = bbox(&lines);
        blocks.push(Block::Paragraph {
            lines,
            bbox,
            role: Role::Body,
        });
    }
    PageLayout { blocks }
}

/// One reading-order segment's spans as lines, top of page first.
fn assemble_lines(spans: &[&TextSpan]) -> Vec<Line> {
    struct Group<'s> {
        y: f32,
        size: f32,
        spans: Vec<&'s TextSpan>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for &span in spans {
        let found = groups
            .iter_mut()
            .find(|group| (group.y - span.y).abs() <= 0.5 * group.size.max(span.size));
        match found {
            Some(group) => {
                group.size = group.size.max(span.size);
                group.spans.push(span);
            }
            None => groups.push(Group {
                y: span.y,
                size: span.size,
                spans: vec![span],
            }),
        }
    }
    groups.sort_by(|a, b| b.y.total_cmp(&a.y)); // top of page first
    groups
        .iter_mut()
        .map(|group| {
            group.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
            assemble_line(group.y, group.size, &group.spans)
        })
        .collect()
}

/// One line from its spans in left-to-right order: a gap wider than
/// [`WORD_GAP`] times the size becomes a space, and a change of
/// `(bold, italic)` opens a new [`Inline`].
fn assemble_line(y: f32, size: f32, spans: &[&TextSpan]) -> Line {
    let mut inlines: Vec<Inline> = Vec::new();
    let mut prev_end: Option<f32> = None;
    let mut prev_size = 0.0f32;
    for span in spans {
        let spaced = prev_end.is_some_and(|end| span.x - end > WORD_GAP * prev_size.max(span.size));
        push_span(&mut inlines, span, spaced);
        prev_end = Some(span.end_x);
        prev_size = span.size;
    }
    Line {
        inlines,
        y,
        x: spans.first().map_or(0.0, |span| span.x),
        end_x: spans.last().map_or(0.0, |span| span.end_x),
        size,
    }
}

/// Extends the run the span continues, or opens one when its style differs.
/// A `spaced` span puts its word-gap space at the end of the run before it,
/// so the space is never lost at a style boundary.
fn push_span(inlines: &mut Vec<Inline>, span: &TextSpan, spaced: bool) {
    if let Some(last) = inlines.last_mut() {
        if spaced {
            last.text.push(' ');
        }
        if last.bold == span.bold && last.italic == span.italic {
            last.text.push_str(&span.text);
            return;
        }
    }
    inlines.push(Inline {
        text: span.text.clone(),
        bold: span.bold,
        italic: span.italic,
    });
}

/// The lines' device-space box. Spans carry no glyph extents, so the top is
/// the highest baseline plus the largest size — an ascender approximation.
fn bbox(lines: &[Line]) -> BBox {
    let mut x0 = f32::INFINITY;
    let mut y0 = f32::INFINITY;
    let mut x1 = f32::NEG_INFINITY;
    let mut y1 = f32::NEG_INFINITY;
    let mut size = 0.0f32;
    for line in lines {
        x0 = x0.min(line.x);
        x1 = x1.max(line.end_x);
        y0 = y0.min(line.y);
        y1 = y1.max(line.y);
        size = size.max(line.size);
    }
    BBox {
        x0,
        y0,
        x1,
        y1: y1 + size,
    }
}

/// The page's spans in reading order, cut into segments.
///
/// Detects a two-column layout by x-occupancy: full-width spans are set
/// aside as band separators, the rest are histogrammed, and the widest
/// empty run whose center sits in the middle of the text width is the
/// gutter candidate. The split only happens when both sides look like
/// real columns (enough spans, enough distinct baselines, enough shared
/// height) — anything less reads top-to-bottom as one segment, which is
/// exactly the old behavior.
fn segments(spans: &[TextSpan]) -> Vec<Vec<&TextSpan>> {
    let whole = || vec![spans.iter().collect::<Vec<&TextSpan>>()];
    let mut x_min = f32::INFINITY;
    let mut x_max = f32::NEG_INFINITY;
    for span in spans {
        x_min = x_min.min(span.x.min(span.end_x));
        x_max = x_max.max(span.x.max(span.end_x));
    }
    let width = x_max - x_min;
    if !width.is_finite() || width <= 0.0 {
        return whole();
    }
    let (separators, body): (Vec<&TextSpan>, Vec<&TextSpan>) = spans
        .iter()
        .partition(|s| (s.end_x - s.x).abs() > SEPARATOR_FRACTION * width);
    if body.len() < COLUMN_MIN_SPANS {
        return whole();
    }
    // Two-column flow lives on portrait-shaped text blocks. A block wider
    // than it is tall is a slide or a table sheet, where a lone lane is a
    // cell boundary, not a gutter.
    let (body_lo, body_hi) = y_extent(&body);
    if body_hi - body_lo <= width {
        return whole();
    }

    let mut occupied = [false; GUTTER_BINS];
    let scale = GUTTER_BINS as f32 / width;
    for span in &body {
        let lo = ((span.x.min(span.end_x) - x_min) * scale).floor().max(0.0) as usize;
        let hi = ((span.x.max(span.end_x) - x_min) * scale).ceil() as usize;
        for bin in occupied.iter_mut().take(hi.min(GUTTER_BINS)).skip(lo) {
            *bin = true;
        }
    }
    // Exactly one wide interior lane is a gutter; several are the cell
    // columns of a data table, whose rows must keep reading left to right.
    let gaps = wide_gaps(&occupied, scale);
    let [gutter] = gaps.as_slice() else {
        return whole();
    };
    let center = (gutter.start + gutter.end) as f32 / 2.0 / GUTTER_BINS as f32;
    if !GUTTER_BAND.contains(&center) {
        return whole();
    }
    let cut = x_min + (gutter.start + gutter.end) as f32 / 2.0 / scale;

    let (left, right): (Vec<&TextSpan>, Vec<&TextSpan>) =
        body.iter().partition(|s| s.x.max(s.end_x) <= cut);
    if !column_shaped(&left) || !column_shaped(&right) {
        return whole();
    }
    if x_span(&left) < COLUMN_MIN_SIDE_WIDTH * width
        || x_span(&right) < COLUMN_MIN_SIDE_WIDTH * width
    {
        return whole();
    }
    let (left_lo, left_hi) = y_extent(&left);
    let (right_lo, right_hi) = y_extent(&right);
    let height = left_hi.max(right_hi) - left_lo.min(right_lo);
    if height <= 0.0
        || left_hi - left_lo < COLUMN_MIN_HEIGHT * height
        || right_hi - right_lo < COLUMN_MIN_HEIGHT * height
    {
        return whole();
    }

    // Bands run top to bottom; each separator line closes the columns
    // above it and reads between them and the columns below.
    let mut cuts: Vec<f32> = separators.iter().map(|s| s.y).collect();
    cuts.sort_by(|a, b| b.total_cmp(a));
    cuts.dedup();
    let mut out: Vec<Vec<&TextSpan>> = Vec::new();
    let mut top = f32::INFINITY;
    for &sep_y in &cuts {
        push_band(&left, &right, top, sep_y, &mut out);
        out.push(
            separators
                .iter()
                .filter(|s| s.y == sep_y)
                .copied()
                .collect(),
        );
        top = sep_y;
    }
    push_band(&left, &right, top, f32::NEG_INFINITY, &mut out);
    out
}

/// Pushes one band's columns — the spans with baseline in `(bottom, top]` —
/// left side first.
fn push_band<'s>(
    left: &[&'s TextSpan],
    right: &[&'s TextSpan],
    top: f32,
    bottom: f32,
    out: &mut Vec<Vec<&'s TextSpan>>,
) {
    for side in [left, right] {
        out.push(
            side.iter()
                .filter(|s| s.y <= top && s.y > bottom)
                .copied()
                .collect(),
        );
    }
}

/// Every interior run of empty bins at least [`GUTTER_MIN_WIDTH`] wide in
/// device space, as half-open bin ranges. Runs touching either edge are
/// margins, not lanes, and are not reported.
fn wide_gaps(occupied: &[bool; GUTTER_BINS], scale: f32) -> Vec<std::ops::Range<usize>> {
    let mut gaps = Vec::new();
    let mut run_start: Option<usize> = None;
    // One trailing filled sentinel closes a run that touches the end.
    let bins = occupied.iter().copied().chain(std::iter::once(true));
    for (i, filled) in bins.enumerate() {
        match (filled, run_start.take()) {
            (false, None) => run_start = Some(i),
            (false, Some(start)) => run_start = Some(start),
            (true, Some(start)) => {
                let interior = start > 0 && i < GUTTER_BINS;
                if interior && (i - start) as f32 / scale >= GUTTER_MIN_WIDTH {
                    gaps.push(start..i);
                }
            }
            (true, None) => {}
        }
    }
    gaps
}

/// True when a gutter side has enough spans on enough distinct baselines
/// to be a text column rather than a stray cluster.
fn column_shaped(spans: &[&TextSpan]) -> bool {
    if spans.len() < COLUMN_MIN_SIDE_SPANS {
        return false;
    }
    let mut baselines: Vec<i32> = spans.iter().map(|s| s.y.round() as i32).collect();
    baselines.sort_unstable();
    baselines.dedup();
    baselines.len() >= COLUMN_MIN_SIDE_LINES
}

/// Lowest and highest baseline of a span set.
fn y_extent(spans: &[&TextSpan]) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for span in spans {
        lo = lo.min(span.y);
        hi = hi.max(span.y);
    }
    (lo, hi)
}

/// Horizontal extent of a span set.
fn x_span(spans: &[&TextSpan]) -> f32 {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for span in spans {
        lo = lo.min(span.x.min(span.end_x));
        hi = hi.max(span.x.max(span.end_x));
    }
    hi - lo
}

/// The pre-IR string builder, kept as the oracle [`layout`] is measured
/// against: it walks segments straight into a `String` with no structure in
/// between. Any divergence is a parity bug in the IR or the [`Text`] adapter.
#[cfg(test)]
pub(crate) fn layout_reference(spans: &[TextSpan]) -> String {
    let mut out = String::new();
    for segment in segments(spans) {
        if segment.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        flow(&segment, &mut out);
    }
    out
}

/// Lays one reading-order segment out into lines, appending to `out`.
#[cfg(test)]
fn flow(spans: &[&TextSpan], out: &mut String) {
    struct Group<'s> {
        y: f32,
        size: f32,
        spans: Vec<&'s TextSpan>,
    }
    let mut lines: Vec<Group> = Vec::new();
    for &span in spans {
        let found = lines
            .iter_mut()
            .find(|line| (line.y - span.y).abs() <= 0.5 * line.size.max(span.size));
        match found {
            Some(line) => {
                line.size = line.size.max(span.size);
                line.spans.push(span);
            }
            None => lines.push(Group {
                y: span.y,
                size: span.size,
                spans: vec![span],
            }),
        }
    }
    lines.sort_by(|a, b| b.y.total_cmp(&a.y)); // top of page first
    for (i, line) in lines.iter_mut().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        line.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut prev_end: Option<f32> = None;
        let mut prev_size = 0.0f32;
        for span in &line.spans {
            if let Some(end) = prev_end {
                let gap = span.x - end;
                if gap > WORD_GAP * prev_size.max(span.size) {
                    out.push(' ');
                }
            }
            out.push_str(&span.text);
            prev_end = Some(span.end_x);
            prev_size = span.size;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pdfboss_core::Document;
    use pdfboss_testkit::doc_with_graphics;

    /// The content streams the crate's Text-adapter parity test replays:
    /// plain lines, both sides of the word-gap threshold, band separators,
    /// and the shapes that must and must not split into columns.
    pub(crate) fn fixture_contents() -> Vec<String> {
        let mut contents: Vec<String> = [
            "BT ET",
            "BT /F1 12 Tf 72 720 Td (Line one) Tj 0 -20 Td (Line two) Tj ET",
            "BT /F1 12 Tf 72 720 Td [(A) -300 (B)] TJ ET",
            "BT /F1 12 Tf 72 720 Td [(A) -50 (B)] TJ ET",
            "BT /F1 12 Tf 0.993 0 0 1 72 720 Tm [(We) -251 (would)] TJ ET",
            "BT /F1 12 Tf 14 TL 72 720 Td (a) Tj T* (b) Tj (c) ' ET",
            "BT /F1 12 Tf 200 720 Td (world) Tj ET BT /F1 12 Tf 72 720 Td (hello) Tj ET",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        contents.push(two_column_content(25));
        contents.push(two_column_content(3));
        contents.push(format!(
            "BT /F1 12 Tf 72 760 Td (A quite wide heading spanning both text columns here) Tj ET {}",
            two_column_content(25)
        ));
        contents
    }

    /// The page's spans, asserting the extraction report is complete: no
    /// test here expects to lose content.
    fn page_spans(doc: &Document, page: &pdfboss_core::Page) -> Vec<TextSpan> {
        let (spans, report) = pdfboss_text::extract_spans_reporting(doc, page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        spans
    }

    /// Extracted, laid-out text of a one-page document with `content` as
    /// its raw content stream (12pt /F1 with default widths of 500).
    fn text_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        layout(&page_spans(&doc, &page))
    }

    #[test]
    fn two_td_lines_become_newline() {
        let text = text_of("BT /F1 12 Tf 72 720 Td (Line one) Tj 0 -20 Td (Line two) Tj ET");
        assert_eq!(text, "Line one\nLine two");
    }

    #[test]
    fn tj_offset_space_thresholds() {
        // -300/1000 * 12 = 3.6 > 0.15 * 12 -> space.
        assert_eq!(
            text_of("BT /F1 12 Tf 72 720 Td [(A) -300 (B)] TJ ET"),
            "A B"
        );
        // -50/1000 * 12 = 0.6 -> no space.
        assert_eq!(text_of("BT /F1 12 Tf 72 720 Td [(A) -50 (B)] TJ ET"), "AB");
    }

    /// Justified LaTeX shrinks inter-word glue below the font's nominal
    /// space width: a Times word gap of 251/1000 em under a slightly
    /// compressed text matrix lands just under 0.25 em in device space,
    /// and a 0.25·size gap threshold reads the whole line as one word.
    #[test]
    fn shrunk_justified_word_gaps_still_become_spaces() {
        let text = text_of("BT /F1 12 Tf 0.993 0 0 1 72 720 Tm [(We) -251 (would)] TJ ET");
        assert_eq!(text, "We would");
    }

    #[test]
    fn invisible_render_mode_still_extracted() {
        assert_eq!(
            text_of("BT /F1 12 Tf 3 Tr 72 720 Td (ghost) Tj ET"),
            "ghost"
        );
    }

    #[test]
    fn leading_and_t_star_and_quote() {
        let text = text_of("BT /F1 12 Tf 14 TL 72 720 Td (a) Tj T* (b) Tj (c) ' ET");
        assert_eq!(text, "a\nb\nc");
    }

    #[test]
    fn layout_orders_spans_left_to_right() {
        let text = text_of(
            "BT /F1 12 Tf 200 720 Td (world) Tj ET \
             BT /F1 12 Tf 72 720 Td (hello) Tj ET",
        );
        assert_eq!(text, "hello world");
    }

    #[test]
    fn empty_content_yields_no_spans() {
        assert_eq!(text_of("BT ET"), "");
    }

    /// One line of four word spans at `x` on baseline `y`, TJ-separated the
    /// way justified text is.
    fn column_line(x: u32, y: u32, tag: &str) -> String {
        format!(
            "BT /F1 12 Tf {x} {y} Td [({tag}a) -400 ({tag}b) -400 ({tag}c) -400 ({tag}d)] TJ ET "
        )
    }

    /// A dense two-column body: `lines` baselines per column, left column at
    /// x=72, right at x=240.
    fn two_column_content(lines: u32) -> String {
        (0..lines)
            .flat_map(|i| {
                let y = 720 - i * 14;
                [
                    column_line(72, y, &format!("L{i}")),
                    column_line(240, y, &format!("R{i}")),
                ]
            })
            .collect()
    }

    /// A page with a clear central gutter reads column-major: the whole left
    /// column, then the whole right column — not line-by-line across both.
    #[test]
    fn two_column_page_reads_column_major() {
        let text = text_of(&two_column_content(25));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "L0a L0b L0c L0d");
        assert_eq!(lines[24], "L24a L24b L24c L24d");
        assert_eq!(lines[25], "R0a R0b R0c R0d");
        assert_eq!(lines[49], "R24a R24b R24c R24d");
    }

    /// A full-width line above the columns is a band separator: it reads
    /// first, and the columns below it still read column-major.
    #[test]
    fn full_width_heading_reads_before_both_columns() {
        let content = format!(
            "BT /F1 12 Tf 72 760 Td (A quite wide heading spanning both text columns here) Tj ET {}",
            two_column_content(25)
        );
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "A quite wide heading spanning both text columns here"
        );
        assert_eq!(lines[1], "L0a L0b L0c L0d");
        assert_eq!(lines[26], "R0a R0b R0c R0d");
    }

    /// Two clusters with too few lines to be columns keep the plain
    /// top-to-bottom, left-to-right order.
    #[test]
    fn sparse_clusters_do_not_split_into_columns() {
        let text = text_of(&two_column_content(3));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "L0a L0b L0c L0d R0a R0b R0c R0d");
    }

    /// A text block wider than it is tall is a slide or a table sheet, not
    /// flowing two-column prose: its lone lane is a cell boundary. Modeled
    /// on a landscape product-overview slide that regressed when the gutter
    /// split first landed.
    #[test]
    fn wide_flat_block_does_not_split() {
        let content: String = (0..12)
            .flat_map(|i| {
                let y = 720 - i * 14;
                [
                    format!("BT /F1 12 Tf 72 {y} Td [(Stagename{i}) -400 (functionaa) -400 (listing)] TJ ET "),
                    format!("BT /F1 12 Tf 400 {y} Td [(Explanation{i}) -400 (of) -400 (the) -400 (feature)] TJ ET "),
                ]
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12);
        assert!(lines[0].starts_with("Stagename0 functionaa listing Explanation0"));
    }

    /// A data table has several full-height empty lanes between its cell
    /// columns where two-column prose has exactly one gutter; picking the
    /// widest lane of a table and splitting there reads the rows
    /// column-major. Modeled on a seven-column registration-results table
    /// that regressed when the gutter split first landed.
    #[test]
    fn multi_lane_table_does_not_split() {
        let content: String = (0..30)
            .map(|i| {
                let y = 720 - i * 14;
                format!(
                    "BT /F1 12 Tf 72 {y} Td (Rowname{i}) Tj ET \
                     BT /F1 12 Tf 200 {y} Td (12345) Tj ET \
                     BT /F1 12 Tf 330 {y} Td (678) Tj ET \
                     BT /F1 12 Tf 430 {y} Td (90) Tj ET "
                )
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 30);
        assert_eq!(lines[0], "Rowname0 12345 678 90");
    }

    /// A table's narrow number column beside a wide text column is not a
    /// two-column layout: rows keep reading left to right. Modeled on a
    /// party-list table that regressed when the gutter split first landed.
    #[test]
    fn narrow_table_column_does_not_split() {
        let content: String = (0..30)
            .map(|i| {
                let y = 720 - i * 14;
                format!(
                    "BT /F1 12 Tf 72 {y} Td (1{i}) Tj ET \
                     BT /F1 12 Tf 300 {y} Td [(Partyaa) -300 (Nameebb) -300 (Row{i})] TJ ET "
                )
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 30);
        assert_eq!(lines[0], "10 Partyaa Nameebb Row0");
    }
}
