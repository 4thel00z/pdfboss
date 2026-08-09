//! Spans to the layout IR: line assembly, word gaps, the two-column gutter
//! split, and the size statistics that rank headings.

use crate::ir::{BBox, Block, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{line_text, Output, Text};
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

/// How far above body size a size bucket must sit before it reads as a
/// heading rather than as emphasis or a stray measurement.
const HEADING_MIN_DELTA: f32 = 1.0;
/// ATX headings stop at six `#`.
const HEADING_MAX_LEVEL: u8 = 6;
/// A wholly bold body-size line this short reads as a title; anything
/// longer is a sentence that happens to be bold.
const BOLD_HEADING_MAX_CHARS: usize = 60;
/// A baseline step beyond this multiple of a run's median step is white
/// space between paragraphs rather than leading inside one.
const PARAGRAPH_GAP: f32 = 1.8;
/// Consecutive heading lines of one size stay one heading while their
/// baseline step is within this multiple of the line size.
const HEADING_MERGE_STEP: f32 = 1.8;

/// The glyphs PDFs draw list bullets with: filled dot, hollow dot, square,
/// en dash, hyphen, asterisk.
const BULLETS: &[char] = &['\u{2022}', '\u{25E6}', '\u{25AA}', '\u{2013}', '-', '*'];
/// A candidate list must total at least this many lines — an item plus a
/// second item, or an item plus one folded continuation — to become a
/// [`Block::List`]. Below it, the lone marker line is a stray dash or
/// bullet-shaped glyph sitting in running prose.
const LIST_MIN_LINES: usize = 2;
/// How far right of its item's marker line a following non-marker line
/// must start, in multiples of the item's size, to read as that item's
/// wrapped continuation rather than the next block.
const LIST_CONTINUATION_INDENT: f32 = 0.5;

/// Groups spans into lines (baselines within `0.5 · size`), orders lines
/// top to bottom and spans left to right, inserts a space at horizontal
/// gaps wider than `WORD_GAP` times the size, and joins lines with `\n`.
/// A page with a clear two-column gutter reads column-major: full-width
/// separators split it into bands, and within each band the left column
/// flows before the right.
pub fn layout(spans: &[TextSpan]) -> String {
    Text.render(&[page_layout(spans)])
}

/// The page's spans as structure, ranking heading sizes against this page
/// alone. Prefer [`document_layout`] whenever the whole document is at
/// hand: a page of nothing but large type has no body size of its own.
pub fn page_layout(spans: &[TextSpan]) -> PageLayout {
    page_layout_with_stats(spans, &size_stats(&[spans]))
}

/// Every page's spans as structure, ranking heading sizes against the whole
/// document, so one oversized page cannot redefine what body text is.
pub fn document_layout(pages: &[Vec<TextSpan>]) -> Vec<PageLayout> {
    let borrowed: Vec<&[TextSpan]> = pages.iter().map(Vec::as_slice).collect();
    let stats = size_stats(&borrowed);
    pages
        .iter()
        .map(|spans| page_layout_with_stats(spans, &stats))
        .collect()
}

/// The page's blocks: each reading-order segment's lines classified into
/// headings and paragraph runs, in order. The classification is a partition
/// — no line is reordered, merged away, or dropped — which is what keeps
/// the [`Text`] adapter byte-equal to positional extraction.
fn page_layout_with_stats(spans: &[TextSpan], stats: &SizeStats) -> PageLayout {
    let mut blocks = Vec::new();
    for segment in segments(spans) {
        if segment.is_empty() {
            continue;
        }
        push_blocks(&assemble_lines(&segment), stats, &mut blocks);
    }
    PageLayout { blocks }
}

/// Character-weighted histogram of span sizes rounded to half a point. Body
/// size is the mode; the ladder holds every distinct bucket at least
/// [`HEADING_MIN_DELTA`] above it, largest first, so a bucket's position is
/// its heading level. The ladder is not cut at six: the buckets nearest body
/// size are the document's real section headings, so ranks past the sixth
/// clamp to level six rather than dropping back to body text.
struct SizeStats {
    body: f32,
    ladder: Vec<f32>,
}

impl SizeStats {
    /// The heading level of a line whose smallest text measures `size`, or
    /// `None` when that size reads as body text.
    fn level(&self, size: f32) -> Option<u8> {
        let rank = self
            .ladder
            .iter()
            .position(|bucket| half_points(*bucket) == half_points(size))?;
        Some(clamped_level(rank + 1))
    }

    /// The level a bold body-size title joins at: one below the ladder's
    /// deepest rank, since it is the smallest heading the page has.
    fn bold_level(&self) -> u8 {
        clamped_level(self.ladder.len() + 1)
    }

    fn is_body(&self, size: f32) -> bool {
        half_points(size) == half_points(self.body)
    }
}

/// A one-based rank as a heading level. Ranks are counted in `usize` because
/// a document may show more distinct sizes than a `u8` can rank.
fn clamped_level(rank: usize) -> u8 {
    rank.min(HEADING_MAX_LEVEL as usize) as u8
}

/// A size as its half-point bucket. Halves are exact in binary, so bucket
/// equality is exact too.
fn half_points(size: f32) -> i32 {
    (size * 2.0).round() as i32
}

/// The document's size statistics, weighted by characters shown: a title
/// carries a handful, body text carries thousands.
fn size_stats(pages: &[&[TextSpan]]) -> SizeStats {
    let mut weights: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    for span in pages.iter().flat_map(|page| page.iter()) {
        *weights.entry(half_points(span.size)).or_default() += span.text.chars().count();
    }
    // Ties go to the smaller size: body text is what a document has most of
    // and, at equal weight, the likelier of the two to be it.
    let body = weights
        .iter()
        .min_by_key(|(bucket, weight)| (std::cmp::Reverse(**weight), **bucket))
        .map(|(bucket, _)| *bucket as f32 / 2.0);
    let Some(body) = body else {
        return SizeStats {
            body: 0.0,
            ladder: Vec::new(),
        };
    };
    let ladder: Vec<f32> = weights
        .keys()
        .rev()
        .map(|bucket| *bucket as f32 / 2.0)
        .filter(|size| *size >= body + HEADING_MIN_DELTA)
        .collect();
    SizeStats { body, ladder }
}

/// One assembled line and the smallest size that put a glyph on it. Heading
/// classification measures a line by its smallest text, so a drop cap or an
/// inline formula cannot promote a body line.
struct Assembled {
    line: Line,
    min_size: f32,
}

/// Emits one segment's lines as blocks, walking them once: a heading line
/// closes the paragraph run before it and takes any tightly-spaced
/// continuation lines of the same size with it.
fn push_blocks(lines: &[Assembled], stats: &SizeStats, out: &mut Vec<Block>) {
    let mut run: Vec<Line> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(level) = heading_level(&lines[index], stats) else {
            run.push(lines[index].line.clone());
            index += 1;
            continue;
        };
        push_run(&mut run, out);
        let mut end = index + 1;
        while end < lines.len() && continues_heading(&lines[end - 1], &lines[end], stats, level) {
            end += 1;
        }
        let heading: Vec<Line> = lines[index..end].iter().map(|a| a.line.clone()).collect();
        let bbox = bbox(&heading);
        out.push(Block::Heading {
            level,
            lines: heading,
            bbox,
        });
        index = end;
    }
    push_run(&mut run, out);
}

/// The heading level of a line: its size's ladder rank, or — for a line at
/// body size — the bold-title rank.
fn heading_level(line: &Assembled, stats: &SizeStats) -> Option<u8> {
    if let Some(level) = stats.level(line.min_size) {
        return Some(level);
    }
    if !stats.is_body(line.min_size) {
        return None;
    }
    if !is_bold_title(&line.line) {
        return None;
    }
    Some(stats.bold_level())
}

/// True when `next` is a wrapped continuation of the heading line `prev`:
/// same size bucket, same level, and no more than a line of space between.
fn continues_heading(prev: &Assembled, next: &Assembled, stats: &SizeStats, level: u8) -> bool {
    if heading_level(next, stats) != Some(level) {
        return false;
    }
    if half_points(prev.min_size) != half_points(next.min_size) {
        return false;
    }
    prev.line.y - next.line.y <= HEADING_MERGE_STEP * next.line.size
}

/// True for a short, wholly bold line that does not end like a sentence —
/// the run-in heading of a document that sets its headings in body size.
fn is_bold_title(line: &Line) -> bool {
    if line.inlines.is_empty() || !line.inlines.iter().all(|inline| inline.bold) {
        return false;
    }
    let text = line_text(line);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > BOLD_HEADING_MAX_CHARS {
        return false;
    }
    !trimmed.ends_with(['.', ',', ';'])
}

/// Splits a heading-free run into blocks: a stretch of marker lines — with
/// hanging continuations folded into their item — becomes one
/// [`Block::List`] once it reaches [`LIST_MIN_LINES`] lines; everything
/// around and between list stretches still goes through [`push_paragraphs`]'
/// gap-based splitting. Detection runs before that split, over the run's
/// lines as assembled, so a list item is never cut into two paragraphs by
/// its own leading.
fn push_run(run: &mut Vec<Line>, out: &mut Vec<Block>) {
    let lines = std::mem::take(run);
    let mut prose: Vec<Line> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(items) = list_run(&lines[index..]) else {
            prose.push(lines[index].clone());
            index += 1;
            continue;
        };
        push_paragraphs(&mut prose, out);
        index += items.iter().map(|item| item.lines.len()).sum::<usize>();
        out.push(Block::List {
            bbox: list_bbox(&items),
            items,
        });
    }
    push_paragraphs(&mut prose, out);
}

/// The list opening at `lines[0]`, or `None` when that line does not open
/// one or the candidate falls short of [`LIST_MIN_LINES`] — in which case
/// the line is left for [`push_run`] to fold back into prose.
fn list_run(lines: &[Line]) -> Option<Vec<ListItem>> {
    let mut items = Vec::new();
    let mut consumed = 0usize;
    let mut index = 0;
    while index < lines.len() {
        let Some((marker, marker_len)) = list_marker(&line_text(&lines[index])) else {
            break;
        };
        let item_x = lines[index].x;
        let item_size = lines[index].size;
        let mut item_lines = vec![lines[index].clone()];
        index += 1;
        while index < lines.len()
            && list_marker(&line_text(&lines[index])).is_none()
            && lines[index].x > item_x + LIST_CONTINUATION_INDENT * item_size
        {
            item_lines.push(lines[index].clone());
            index += 1;
        }
        consumed += item_lines.len();
        items.push(ListItem {
            marker,
            marker_len,
            lines: item_lines,
        });
    }
    (consumed >= LIST_MIN_LINES).then_some(items)
}

/// Some(marker, marker_len) when `text` opens a list item: a bullet from
/// [`BULLETS`] followed by whitespace, or 1-3 digits then `.`/`)` then
/// whitespace. `marker_len` is the matched prefix's length in characters.
/// A bare number with nothing after it is not an item.
fn list_marker(text: &str) -> Option<(Marker, usize)> {
    let trimmed = text.trim_start();
    let indent = text.chars().count() - trimmed.chars().count();
    let first = trimmed.chars().next()?;
    if BULLETS.contains(&first) {
        let rest = &trimmed[first.len_utf8()..];
        let whitespace = rest.chars().take_while(|c| c.is_whitespace()).count();
        if whitespace == 0 {
            return None;
        }
        return Some((Marker::Bullet, indent + 1 + whitespace));
    }
    if !first.is_ascii_digit() {
        return None;
    }
    let digit_count = trimmed
        .chars()
        .take(3)
        .take_while(char::is_ascii_digit)
        .count();
    let rest = &trimmed[digit_count..];
    let mut rest_chars = rest.chars();
    let separator = rest_chars.next()?;
    if separator != '.' && separator != ')' {
        return None;
    }
    let whitespace = rest_chars
        .as_str()
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    if whitespace == 0 {
        return None;
    }
    let number: u32 = trimmed[..digit_count].parse().ok()?;
    Some((
        Marker::Number(number),
        indent + digit_count + 1 + whitespace,
    ))
}

/// The list's device-space box: every item's lines combined.
fn list_bbox(items: &[ListItem]) -> BBox {
    let lines: Vec<Line> = items.iter().flat_map(|item| item.lines.clone()).collect();
    bbox(&lines)
}

/// Drains a paragraph run into blocks, cutting it where a baseline step
/// exceeds [`PARAGRAPH_GAP`] times the run's median step.
fn push_paragraphs(run: &mut Vec<Line>, out: &mut Vec<Block>) {
    let lines = std::mem::take(run);
    if lines.is_empty() {
        return;
    }
    let limit = PARAGRAPH_GAP * median_step(&lines);
    let mut start = 0;
    for index in 1..lines.len() {
        if limit <= 0.0 || lines[index - 1].y - lines[index].y <= limit {
            continue;
        }
        push_paragraph(&lines[start..index], out);
        start = index;
    }
    push_paragraph(&lines[start..], out);
}

fn push_paragraph(lines: &[Line], out: &mut Vec<Block>) {
    if lines.is_empty() {
        return;
    }
    out.push(Block::Paragraph {
        lines: lines.to_vec(),
        bbox: bbox(lines),
        role: Role::Body,
    });
}

/// Median baseline step of consecutive lines, or zero when there is no step
/// to measure — a run of one line never splits.
fn median_step(lines: &[Line]) -> f32 {
    let mut steps: Vec<f32> = lines.windows(2).map(|pair| pair[0].y - pair[1].y).collect();
    if steps.is_empty() {
        return 0.0;
    }
    steps.sort_by(f32::total_cmp);
    steps[steps.len() / 2]
}

/// One reading-order segment's spans as lines, top of page first.
fn assemble_lines(spans: &[&TextSpan]) -> Vec<Assembled> {
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
fn assemble_line(y: f32, size: f32, spans: &[&TextSpan]) -> Assembled {
    let mut inlines: Vec<Inline> = Vec::new();
    let mut prev_end: Option<f32> = None;
    let mut prev_size = 0.0f32;
    let mut min_size = f32::INFINITY;
    for span in spans {
        let spaced = prev_end.is_some_and(|end| span.x - end > WORD_GAP * prev_size.max(span.size));
        push_span(&mut inlines, span, spaced);
        prev_end = Some(span.end_x);
        prev_size = span.size;
        min_size = min_size.min(span.size);
    }
    Assembled {
        line: Line {
            inlines,
            y,
            x: spans.first().map_or(0.0, |span| span.x),
            end_x: spans.last().map_or(0.0, |span| span.end_x),
            size,
        },
        min_size: if min_size.is_finite() { min_size } else { size },
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
    /// the shapes that must and must not split into columns, and a page whose
    /// sizes and spacing put headings and a paragraph break into the
    /// partition — Text output must survive all of it unchanged.
    pub(crate) fn fixture_contents() -> Vec<String> {
        let mut contents: Vec<String> = [
            "BT ET",
            "BT /F1 12 Tf 72 720 Td (Line one) Tj 0 -20 Td (Line two) Tj ET",
            "BT /F1 12 Tf 72 720 Td [(A) -300 (B)] TJ ET",
            "BT /F1 12 Tf 72 720 Td [(A) -50 (B)] TJ ET",
            "BT /F1 12 Tf 0.993 0 0 1 72 720 Tm [(We) -251 (would)] TJ ET",
            "BT /F1 12 Tf 14 TL 72 720 Td (a) Tj T* (b) Tj (c) ' ET",
            "BT /F1 12 Tf 200 720 Td (world) Tj ET BT /F1 12 Tf 72 720 Td (hello) Tj ET",
            "BT /F1 24 Tf 72 740 Td (Chapter title) Tj \
             /F1 12 Tf 0 -40 Td (Body line one is long enough to look like body.) Tj \
             0 -14 Td (Body line two keeps twelve the dominant size.) Tj \
             0 -14 Td (And a third line for good measure.) Tj \
             0 -60 Td (A far-below line starts a second paragraph.) Tj ET",
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
