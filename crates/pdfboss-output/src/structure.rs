//! Spans to the layout IR: line assembly, word gaps, the two-column gutter
//! split, and the size statistics that rank headings.

use crate::ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{line_text, Output, Text};
use pdfboss_text::{Ruling, TextSpan};

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
/// A heading names a section, so it is short. Past this many characters the
/// large type is a pull quote, a caption, or — on a page whose body size the
/// character histogram read off a dense table — ordinary prose one bucket up.
const HEADING_MAX_CHARS: usize = 120;
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

/// A grid needs a lane between every pair of cell columns, so two lanes —
/// three cell columns — is the narrowest band that reads as a table. One
/// lane is a gutter, a label beside its value, or a hanging indent.
const TABLE_MIN_LANES: usize = 2;
/// How many rows must populate [`TABLE_MIN_ROW_CELLS`] cells before the band
/// is a table rather than two lines that happen to share a lane.
const TABLE_MIN_ROWS: usize = 3;
const TABLE_MIN_ROW_CELLS: usize = 2;
/// A baseline step beyond this multiple of the band's median step is white
/// space between blocks rather than the next row.
const TABLE_ROW_GAP: f32 = 2.0;

/// How close two rulings must sit to read as one drawn line: collinear
/// segments cluster within it, and a lattice crossing may miss by it.
/// Six points, because tables are often drawn one row box at a time with
/// the side borders stopping five and a half points short of the next
/// row's rule — the corners must still weld into one lattice.
const RULING_SNAP_TOLERANCE: f32 = 6.0;
/// The narrowest lattice that reads as a ruled grid: two verticals and three
/// horizontals are one boxed column of two cells. Lane-occupancy gates do not
/// apply here — the structure is drawn, not implied by white space.
const RULED_GRID_MIN_VERTICALS: usize = 2;
const RULED_GRID_MIN_HORIZONTALS: usize = 3;
/// Populated bands a fully boxed grid needs; an unboxed lattice needs
/// [`TABLE_MIN_ROWS`], since stray separators reach three lines more easily
/// than a drawn border box does.
const RULED_BOXED_MIN_ROWS: usize = 2;

/// Minimum pages before a repeated edge line reads as a running line rather than
/// coincidence: two documents opening with the same word is unremarkable,
/// three or more sharing a whole line is not.
const HEADER_FOOTER_MIN_PAGES: usize = 3;
/// How close two occurrences' baselines must sit to read as the same running
/// line rather than two different ones that happen to match text.
const HEADER_FOOTER_Y_TOLERANCE: f32 = 2.0;

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
    page_layout_with_rulings(spans, &[])
}

/// [`page_layout`] with the page's rulings: a lattice of drawn borders is
/// read as a table ahead of lane occupancy. With no rulings the two are the
/// same function.
pub fn page_layout_with_rulings(spans: &[TextSpan], rulings: &[Ruling]) -> PageLayout {
    page_layout_with_stats(spans, rulings, &size_stats(&[spans]))
}

/// Every page's spans as structure, ranking heading sizes against the whole
/// document, so one oversized page cannot redefine what body text is.
pub fn document_layout(pages: &[Vec<TextSpan>]) -> Vec<PageLayout> {
    let paired: Vec<(&[TextSpan], &[Ruling])> = pages
        .iter()
        .map(|spans| (spans.as_slice(), &[][..]))
        .collect();
    layouts_of(&paired)
}

/// [`document_layout`] with each page's rulings, so drawn grids become
/// tables document-wide. With no rulings the two are the same function.
pub fn document_layout_with_rulings(pages: &[(Vec<TextSpan>, Vec<Ruling>)]) -> Vec<PageLayout> {
    let paired: Vec<(&[TextSpan], &[Ruling])> = pages
        .iter()
        .map(|(spans, rulings)| (spans.as_slice(), rulings.as_slice()))
        .collect();
    layouts_of(&paired)
}

/// Every page's layout over shared document-wide size statistics.
fn layouts_of(pages: &[(&[TextSpan], &[Ruling])]) -> Vec<PageLayout> {
    let borrowed: Vec<&[TextSpan]> = pages.iter().map(|(spans, _)| *spans).collect();
    let stats = size_stats(&borrowed);
    let mut layouts: Vec<PageLayout> = pages
        .iter()
        .map(|(spans, rulings)| page_layout_with_stats(spans, rulings, &stats))
        .collect();
    tag_page_roles(&mut layouts);
    layouts
}

/// Tags page headers and footers: a page's first or last line, repeated near-verbatim
/// at the same baseline across enough pages, is split out of whatever
/// paragraph it was assembled into and marked `PageHeader`/`PageFooter`; a
/// line that is nothing but a page number is tagged on its own, with no
/// repetition required. Needs at least [`HEADER_FOOTER_MIN_PAGES`] pages —
/// below that, a repeat is coincidence as often as a real running line.
fn tag_page_roles(layouts: &mut [PageLayout]) {
    if layouts.len() < HEADER_FOOTER_MIN_PAGES {
        return;
    }
    let top: Vec<Option<(String, f32)>> = layouts
        .iter()
        .map(|layout| edge_line(layout, true))
        .collect();
    let bottom: Vec<Option<(String, f32)>> = layouts
        .iter()
        .map(|layout| edge_line(layout, false))
        .collect();
    let headers = header_footer_pages(&top);
    let footers = header_footer_pages(&bottom);
    for (index, layout) in layouts.iter_mut().enumerate() {
        // Footer first: on a one-block page that qualifies as both, the
        // block can only be split once, and the first split wins.
        if footers[index] {
            split_edge(layout, false, Role::PageFooter);
        }
        if headers[index] {
            split_edge(layout, true, Role::PageHeader);
        }
    }
}

/// The page's first (`top`) or last line, normalized, and its baseline —
/// the shape a running header or a page number takes — when the block it
/// sits in is an untagged `Paragraph`.
fn edge_line(layout: &PageLayout, top: bool) -> Option<(String, f32)> {
    let block = if top {
        layout.blocks.first()
    } else {
        layout.blocks.last()
    }?;
    let Block::Paragraph { lines, role, .. } = block else {
        return None;
    };
    if !matches!(role, Role::Body) {
        return None;
    }
    let line = if top { lines.first() } else { lines.last() }?;
    let normalized = normalize_candidate(&line_text(line));
    (!normalized.is_empty()).then_some((normalized, line.y))
}

/// Which pages' edge-line candidates should be tagged header/footer: repetition
/// of the same normalized text at a close enough baseline on at least
/// `max(HEADER_FOOTER_MIN_PAGES, pages / 2)` pages, or — with no repetition
/// required — a line that is nothing but a page number.
fn header_footer_pages(candidates: &[Option<(String, f32)>]) -> Vec<bool> {
    let mut tagged = repeated_lines(candidates);
    for (index, candidate) in candidates.iter().enumerate() {
        let Some((text, _)) = candidate else { continue };
        tagged[index] |= looks_like_page_number(text);
    }
    tagged
}

/// Pages whose edge line's normalized text repeats, at a close enough
/// baseline, on enough other pages.
fn repeated_lines(candidates: &[Option<(String, f32)>]) -> Vec<bool> {
    let threshold = (candidates.len() / 2).max(HEADER_FOOTER_MIN_PAGES);
    let mut groups: std::collections::BTreeMap<&str, Vec<(usize, f32)>> =
        std::collections::BTreeMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let Some((text, y)) = candidate else { continue };
        groups.entry(text.as_str()).or_default().push((index, *y));
    }
    let mut tagged = vec![false; candidates.len()];
    for occurrences in groups.values() {
        if occurrences.len() < threshold {
            continue;
        }
        let (min_y, max_y) = occurrences
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &(_, y)| {
                (lo.min(y), hi.max(y))
            });
        if max_y - min_y > HEADER_FOOTER_Y_TOLERANCE {
            continue;
        }
        for &(index, _) in occurrences {
            tagged[index] = true;
        }
    }
    tagged
}

/// Case- and digit-blind text for repetition matching: a running header and
/// a page number repeat their shape on every page, not always their exact
/// characters.
fn normalize_candidate(text: &str) -> String {
    let digits_marked: String = text
        .chars()
        .map(|ch| if ch.is_ascii_digit() { '#' } else { ch })
        .collect();
    digits_marked
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

/// A normalized line that is nothing but a page number: `#`, `page #`,
/// `# of #`, `page # of #`, or `- # -`. A page number's text changes on
/// every page, so it is tagged on its own rather than by repetition.
fn looks_like_page_number(normalized: &str) -> bool {
    let body = normalized.strip_prefix("page ").unwrap_or(normalized);
    if is_hash_run(body) {
        return true;
    }
    if let Some((left, right)) = body.split_once(" of ") {
        return is_hash_run(left) && is_hash_run(right);
    }
    let Some(inner) = normalized
        .strip_prefix('-')
        .and_then(|s| s.strip_suffix('-'))
    else {
        return false;
    };
    is_hash_run(inner.trim())
}

/// Non-empty and made of nothing but `#`.
fn is_hash_run(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|ch| ch == '#')
}

/// Splits the page's first (`top`) or last block's edge line into its own
/// single-line `Paragraph` tagged `role`, leaving the rest of that block as
/// `Body`. A no-op when the block at that end is not an untagged
/// `Paragraph` — including when the opposite end's split already claimed it.
fn split_edge(layout: &mut PageLayout, top: bool, role: Role) {
    if layout.blocks.is_empty() {
        return;
    }
    let index = if top { 0 } else { layout.blocks.len() - 1 };
    let Block::Paragraph {
        lines,
        role: current,
        ..
    } = &layout.blocks[index]
    else {
        return;
    };
    if !matches!(current, Role::Body) {
        return;
    }
    let mut lines = lines.clone();
    let edge_line = if top {
        lines.remove(0)
    } else {
        let Some(line) = lines.pop() else { return };
        line
    };
    let edge_block = Block::Paragraph {
        bbox: bbox(std::slice::from_ref(&edge_line)),
        lines: vec![edge_line],
        role,
    };
    if lines.is_empty() {
        layout.blocks[index] = edge_block;
        return;
    }
    let rest_block = Block::Paragraph {
        bbox: bbox(&lines),
        lines,
        role: Role::Body,
    };
    if top {
        layout.blocks[index] = rest_block;
        layout.blocks.insert(index, edge_block);
    } else {
        layout.blocks[index] = rest_block;
        layout.blocks.push(edge_block);
    }
}

/// The page's blocks: each reading-order segment's lines classified into
/// headings and paragraph runs, in order. The classification is a partition
/// — no line is reordered, merged away, or dropped — which is what keeps
/// the [`Text`] adapter byte-equal to positional extraction.
fn page_layout_with_stats(spans: &[TextSpan], rulings: &[Ruling], stats: &SizeStats) -> PageLayout {
    let grids = ruled_grids(rulings);
    let mut blocks = Vec::new();
    for segment in segments(spans) {
        if segment.is_empty() {
            continue;
        }
        if let Some((band, bbox)) = ruled_band(&segment, &grids) {
            push_blocks(&band.above, stats, &mut blocks);
            blocks.push(Block::Table {
                bbox,
                rows: band.rows,
            });
            push_blocks(&band.below, stats, &mut blocks);
            continue;
        }
        if let Some(band) = table_band(&segment) {
            push_blocks(&band.above, stats, &mut blocks);
            blocks.push(Block::Table {
                bbox: table_bbox(&band.rows),
                rows: band.rows,
            });
            push_blocks(&band.below, stats, &mut blocks);
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
        let mut end = index + 1;
        while end < lines.len() && continues_heading(&lines[end - 1], &lines[end], stats, level) {
            end += 1;
        }
        let heading: Vec<Line> = lines[index..end].iter().map(|a| a.line.clone()).collect();
        if heading_chars(&heading) > HEADING_MAX_CHARS {
            run.extend(heading);
            index = end;
            continue;
        }
        push_run(&mut run, out);
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

/// A candidate heading's text length, counted as the Markdown adapter joins
/// its lines: one space between them, ends trimmed.
fn heading_chars(lines: &[Line]) -> usize {
    lines
        .iter()
        .map(line_text)
        .collect::<Vec<String>>()
        .join(" ")
        .trim()
        .chars()
        .count()
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
    median(lines.windows(2).map(|pair| pair[0].y - pair[1].y).collect())
}

/// The middle value, or zero when there is none.
fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

/// The spans that share one visual line, and the line's baseline and largest
/// size. Table rows are these groups too, which is what makes a table's rows
/// the very lines the flat flow would have written.
struct Group<'s> {
    y: f32,
    size: f32,
    spans: Vec<&'s TextSpan>,
}

/// One reading-order segment's spans grouped into lines — baselines within
/// `0.5 · size` — top of page first, spans left to right inside each.
fn line_groups<'s>(spans: &[&'s TextSpan]) -> Vec<Group<'s>> {
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
    for group in &mut groups {
        group.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    groups
}

/// One reading-order segment's spans as lines, top of page first.
fn assemble_lines(spans: &[&TextSpan]) -> Vec<Assembled> {
    line_groups(spans).iter().map(assembled).collect()
}

/// A grid and the page-edge lines around it: the lines above the first
/// populated row and below the last are a caption, a running header or a page
/// number, not rows.
struct TableBand {
    above: Vec<Assembled>,
    rows: Vec<Vec<Cell>>,
    below: Vec<Assembled>,
}

/// A lattice of drawn rulings: the x positions of its vertical lines and the
/// y positions of its horizontal lines, ascending, joined by their crossings
/// into one connected region. Built by [`ruled_grids`].
struct RuledGrid {
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// Whether all four outer borders are drawn end to end.
    boxed: bool,
}

impl RuledGrid {
    /// The x ranges between consecutive vertical lines: the grid's cell
    /// columns, in the shape [`table_row`] takes.
    fn columns(&self) -> Vec<std::ops::Range<f32>> {
        self.xs.windows(2).map(|pair| pair[0]..pair[1]).collect()
    }

    /// True when a baseline at `y` sits inside the grid: on or above the
    /// bottom border, strictly below the top one — a baseline exactly on a
    /// ruling has its glyphs in the band above it.
    fn holds(&self, y: f32) -> bool {
        self.ys[0] <= y && y < self.ys[self.ys.len() - 1]
    }

    /// The index of the band between consecutive horizontal rulings a
    /// baseline at `y` falls in; callers check [`RuledGrid::holds`] first.
    fn band_of(&self, y: f32) -> usize {
        self.ys.partition_point(|ruling_y| *ruling_y <= y) - 1
    }

    /// The grid's drawn border box.
    fn bbox(&self) -> BBox {
        BBox {
            x0: self.xs[0],
            y0: self.ys[0],
            x1: self.xs[self.xs.len() - 1],
            y1: self.ys[self.ys.len() - 1],
        }
    }
}

/// Collinear rulings merged into one drawn line: the position on the
/// constant axis and the extent covered along the other.
struct GridLine {
    position: f32,
    extent: std::ops::Range<f32>,
}

/// The page's ruled grids: vertical and horizontal rulings clustered into
/// drawn lines, lines joined where they cross, and every connected lattice
/// of at least [`RULED_GRID_MIN_VERTICALS`] verticals and
/// [`RULED_GRID_MIN_HORIZONTALS`] horizontals kept, topmost first. A ruling
/// is vertical when it runs farther in y than in x; extraction already
/// snapped it exactly axis-aligned.
fn ruled_grids(rulings: &[Ruling]) -> Vec<RuledGrid> {
    if rulings.is_empty() {
        return Vec::new();
    }
    let vertical = |r: &&Ruling| r.end.y - r.start.y > r.end.x - r.start.x;
    let verticals = grid_lines(
        rulings
            .iter()
            .filter(vertical)
            .map(|r| (r.start.x, r.start.y..r.end.y)),
    );
    let horizontals = grid_lines(
        rulings
            .iter()
            .filter(|r| !vertical(r))
            .map(|r| (r.start.y, r.start.x..r.end.x)),
    );
    let mut parent: Vec<usize> = (0..verticals.len() + horizontals.len()).collect();
    for (v, vertical) in verticals.iter().enumerate() {
        for (h, horizontal) in horizontals.iter().enumerate() {
            if crosses(vertical, horizontal) {
                union(&mut parent, v, verticals.len() + h);
            }
        }
    }
    let mut components: std::collections::BTreeMap<usize, (Vec<usize>, Vec<usize>)> =
        std::collections::BTreeMap::new();
    for v in 0..verticals.len() {
        let root = find(&mut parent, v);
        components.entry(root).or_default().0.push(v);
    }
    for h in 0..horizontals.len() {
        let root = find(&mut parent, verticals.len() + h);
        components.entry(root).or_default().1.push(h);
    }
    let mut grids: Vec<RuledGrid> = components
        .values()
        .filter_map(|(v_indices, h_indices)| {
            let component_verticals: Vec<&GridLine> =
                v_indices.iter().map(|&i| &verticals[i]).collect();
            let component_horizontals: Vec<&GridLine> =
                h_indices.iter().map(|&i| &horizontals[i]).collect();
            lattice(&component_verticals, &component_horizontals)
        })
        .collect();
    grids.sort_by(|a, b| b.ys[b.ys.len() - 1].total_cmp(&a.ys[a.ys.len() - 1]));
    grids
}

/// Rulings on one axis as drawn lines: grouped by their constant coordinate
/// within [`RULING_SNAP_TOLERANCE`], each group's near-touching extents
/// merged. Extents farther apart stay separate lines at the same position —
/// the x two stacked tables' borders share must not weld them into one
/// lattice.
fn grid_lines(rulings: impl Iterator<Item = (f32, std::ops::Range<f32>)>) -> Vec<GridLine> {
    let mut all: Vec<(f32, std::ops::Range<f32>)> = rulings.collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut lines: Vec<GridLine> = Vec::new();
    let mut cluster: Vec<(f32, std::ops::Range<f32>)> = Vec::new();
    for line in all {
        if let Some(last) = cluster.last() {
            if line.0 - last.0 > RULING_SNAP_TOLERANCE {
                lines.append(&mut merged_cluster(std::mem::take(&mut cluster)));
            }
        }
        cluster.push(line);
    }
    lines.append(&mut merged_cluster(cluster));
    lines
}

/// One position cluster's segments as [`GridLine`]s at the cluster's mean
/// position: extents that overlap or come within [`RULING_SNAP_TOLERANCE`]
/// merge, the rest stay separate lines.
fn merged_cluster(mut cluster: Vec<(f32, std::ops::Range<f32>)>) -> Vec<GridLine> {
    if cluster.is_empty() {
        return Vec::new();
    }
    let position = cluster.iter().map(|(p, _)| *p).sum::<f32>() / cluster.len() as f32;
    cluster.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
    let mut lines: Vec<GridLine> = Vec::new();
    for (_, extent) in cluster {
        match lines.last_mut() {
            Some(last) if extent.start <= last.extent.end + RULING_SNAP_TOLERANCE => {
                last.extent.end = last.extent.end.max(extent.end);
            }
            _ => lines.push(GridLine { position, extent }),
        }
    }
    lines
}

/// True when a vertical and a horizontal line cross within
/// [`RULING_SNAP_TOLERANCE`]: an L corner, a T junction, or a + crossing.
fn crosses(vertical: &GridLine, horizontal: &GridLine) -> bool {
    vertical.position >= horizontal.extent.start - RULING_SNAP_TOLERANCE
        && vertical.position <= horizontal.extent.end + RULING_SNAP_TOLERANCE
        && horizontal.position >= vertical.extent.start - RULING_SNAP_TOLERANCE
        && horizontal.position <= vertical.extent.end + RULING_SNAP_TOLERANCE
}

/// The set representative, with path halving.
fn find(parent: &mut [usize], mut node: usize) -> usize {
    while parent[node] != node {
        parent[node] = parent[parent[node]];
        node = parent[node];
    }
    node
}

/// Joins the two nodes' sets.
fn union(parent: &mut [usize], a: usize, b: usize) {
    let root_a = find(parent, a);
    let root_b = find(parent, b);
    parent[root_a] = root_b;
}

/// One connected component as a grid, or `None` when it is too sparse to be
/// one: a lone separator, an underline, a pair of column rules with nothing
/// across them.
fn lattice(verticals: &[&GridLine], horizontals: &[&GridLine]) -> Option<RuledGrid> {
    let xs = distinct_positions(verticals);
    let ys = distinct_positions(horizontals);
    if xs.len() < RULED_GRID_MIN_VERTICALS || ys.len() < RULED_GRID_MIN_HORIZONTALS {
        return None;
    }
    let (x_lo, x_hi) = (xs[0], xs[xs.len() - 1]);
    let (y_lo, y_hi) = (ys[0], ys[ys.len() - 1]);
    let boxed = covers(verticals, x_lo, y_lo, y_hi)
        && covers(verticals, x_hi, y_lo, y_hi)
        && covers(horizontals, y_lo, x_lo, x_hi)
        && covers(horizontals, y_hi, x_lo, x_hi);
    Some(RuledGrid { xs, ys, boxed })
}

/// The lines' positions, ascending, neighbours within
/// [`RULING_SNAP_TOLERANCE`] collapsed to the first.
fn distinct_positions(lines: &[&GridLine]) -> Vec<f32> {
    let mut positions: Vec<f32> = lines.iter().map(|line| line.position).collect();
    positions.sort_by(f32::total_cmp);
    positions.dedup_by(|next, kept| *next - *kept <= RULING_SNAP_TOLERANCE);
    positions
}

/// True when the lines at `position` cover `lo..hi` end to end, gaps and
/// shortfalls no wider than [`RULING_SNAP_TOLERANCE`]: whether a border is
/// drawn along the whole of one edge.
fn covers(lines: &[&GridLine], position: f32, lo: f32, hi: f32) -> bool {
    let mut extents: Vec<&std::ops::Range<f32>> = lines
        .iter()
        .filter(|line| (line.position - position).abs() <= RULING_SNAP_TOLERANCE)
        .map(|line| &line.extent)
        .collect();
    extents.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut reached = lo;
    for extent in extents {
        if extent.start > reached + RULING_SNAP_TOLERANCE {
            return false;
        }
        reached = reached.max(extent.end);
    }
    reached >= hi - RULING_SNAP_TOLERANCE
}

/// The segment's lines read against the page's drawn grids: the first grid,
/// topmost first, that holds the segment's interior lines as rows, as a
/// [`TableBand`] plus the grid's drawn border box. Lane-occupancy gates do
/// not apply — a drawn single-column box is a table no lane could show — but
/// every row still passes [`table_row`]'s word-gap cell gate, so a `None`
/// falls through to the lane path with the flat flow intact.
fn ruled_band(segment: &[&TextSpan], grids: &[RuledGrid]) -> Option<(TableBand, BBox)> {
    if grids.is_empty() {
        return None;
    }
    let groups = line_groups(segment);
    grids.iter().find_map(|grid| grid_band(&groups, grid))
}

/// `groups` against one grid: the contiguous stretch of lines whose
/// baselines fall inside the grid becomes its rows; the lines above and
/// below stay prose. `None` when a line will not sit in the grid's columns,
/// when a column boundary lands inside a sub-word gap — the row would split
/// a word the flat flow wrote whole — or when the populated bands between
/// horizontal rulings number under [`TABLE_MIN_ROWS`], or under
/// [`RULED_BOXED_MIN_ROWS`] for a grid with all four borders drawn.
fn grid_band(groups: &[Group], grid: &RuledGrid) -> Option<(TableBand, BBox)> {
    let lo = groups.iter().position(|group| grid.holds(group.y))?;
    let inside = groups[lo..]
        .iter()
        .take_while(|group| grid.holds(group.y))
        .count();
    let hi = lo + inside;
    let columns = grid.columns();
    let mut rows = Vec::with_capacity(inside);
    let mut bands = std::collections::BTreeSet::new();
    for group in &groups[lo..hi] {
        rows.push(table_row(group, &columns)?);
        bands.insert(grid.band_of(group.y));
    }
    if bands.len() < TABLE_MIN_ROWS && !(grid.boxed && bands.len() >= RULED_BOXED_MIN_ROWS) {
        return None;
    }
    let above = groups[..lo].iter().map(assembled).collect();
    let below = groups[hi..].iter().map(assembled).collect();
    Some((TableBand { above, rows, below }, grid.bbox()))
}

/// The longest stretch of the segment's lines that reads as a grid, or `None`
/// when no stretch does and the whole segment must flow as prose.
///
/// Lanes are measured over the candidate stretch alone, never over the whole
/// segment: a page title and a paragraph of prose put ink across the width the
/// grid keeps clear, so a segment holding anything besides its table leaves no
/// lanes at all. Adding a line can only fill bins, so lanes shrink as a stretch
/// grows and never come back — a stretch is grown until they fall below
/// [`TABLE_MIN_LANES`], and that is its end. A wrapped cell standing alone in
/// one column survives inside the stretch for free: it occupies bins that
/// column already held.
fn table_band(segment: &[&TextSpan]) -> Option<TableBand> {
    let groups = line_groups(segment);
    let (x_min, x_max) = x_bounds(segment);
    let width = x_max - x_min;
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let scale = GUTTER_BINS as f32 / width;
    for start in 0..groups.len() {
        let (end, lanes) = lane_run(&groups, start, x_min, scale);
        if end - start < TABLE_MIN_ROWS {
            continue;
        }
        if let Some(band) = grid(&groups, start, end, &lanes) {
            return Some(band);
        }
    }
    None
}

/// The stretch starting at `start` that keeps at least [`TABLE_MIN_LANES`]
/// lanes, as an exclusive end and the lanes the whole stretch leaves. The
/// occupancy histogram is kept in the segment's frame so each line's bins can
/// simply be added to it; a stretch narrower than the segment leaves its
/// margins empty, and [`wide_gaps`] already reads edge runs as margins.
fn lane_run(
    groups: &[Group],
    start: usize,
    x_min: f32,
    scale: f32,
) -> (usize, Vec<std::ops::Range<f32>>) {
    let mut occupied = [false; GUTTER_BINS];
    let mut lanes = Vec::new();
    for (offset, group) in groups[start..].iter().enumerate() {
        let mut next = occupied;
        fill_bins(&mut next, &group.spans, x_min, scale);
        let gaps = wide_gaps(&next, scale);
        if gaps.len() < TABLE_MIN_LANES {
            return (start + offset, lanes);
        }
        occupied = next;
        lanes = lane_ranges(&gaps, x_min, scale);
    }
    (groups.len(), lanes)
}

/// `groups[start..end]` as a table band, or `None` when it fails a gate.
///
/// The gates are all required: three cell columns, three rows populating two
/// cells each, every span sitting in a column, evenly spaced row baselines,
/// and neighbouring cells more than a word gap apart — the last so a row
/// reads as the one line the flat flow wrote, cell texts and all.
///
/// The grid runs from the first populated row to the last, so its first row
/// is a real one. Lines outside that stretch populate a single cell and leave
/// as prose — where the roles and the heading and list passes can still read
/// them; the single-cell lines inside it stay rows, being the wrapped cells
/// and continuation lines of the grid itself. The column gate is then asked
/// again of what is left, because hoisting those edge lines can take the only
/// text a column ever held.
fn grid(
    groups: &[Group],
    start: usize,
    end: usize,
    lanes: &[std::ops::Range<f32>],
) -> Option<TableBand> {
    let spans: Vec<&TextSpan> = groups[start..end]
        .iter()
        .flat_map(|group| group.spans.iter().copied())
        .collect();
    let columns = cell_columns(&spans, lanes);
    let (lo, hi) = merged_edges(groups, start, end, &columns);
    let inside = &groups[lo..hi];
    let mut rows = Vec::with_capacity(inside.len());
    let mut populated = Vec::with_capacity(inside.len());
    for group in inside {
        let Some(row) = table_row(group, &columns) else {
            break;
        };
        let cells = row.iter().filter(|cell| cell.line.is_some()).count();
        populated.push(cells >= TABLE_MIN_ROW_CELLS);
        rows.push(row);
    }
    let first = populated.iter().position(|filled| *filled)?;
    let last = populated.iter().rposition(|filled| *filled)?;
    let baselines: Vec<f32> = inside[first..=last]
        .iter()
        .zip(&populated[first..=last])
        .filter(|(_, filled)| **filled)
        .map(|(group, _)| group.y)
        .collect();
    if baselines.len() < TABLE_MIN_ROWS {
        return None;
    }
    if !even_rows(&baselines) {
        return None;
    }
    if populated_columns(&rows[first..=last], columns.len()) < TABLE_MIN_LANES + 1 {
        return None;
    }
    let above = groups[..lo + first].iter().map(assembled).collect();
    let below = groups[lo + last + 1..].iter().map(assembled).collect();
    rows.truncate(last + 1);
    rows.drain(..first);
    Some(TableBand { above, rows, below })
}

/// The stretch `start..end` grown over the neighbouring lines that still sit
/// in `columns`, as a half-open range. A header or total row whose cell covers
/// two columns puts ink in the lane between them and so ends the lane run
/// short of itself, but it is a row of this grid all the same — `table_row`
/// reads it as the merged cell it is. Growth stops at a line the columns
/// cannot hold, and at one standing further off than [`TABLE_ROW_GAP`] times
/// the run's own row pitch, which is a separate block that happens to fit.
fn merged_edges(
    groups: &[Group],
    start: usize,
    end: usize,
    columns: &[std::ops::Range<f32>],
) -> (usize, usize) {
    let pitch = median(
        groups[start..end]
            .windows(2)
            .map(|pair| pair[0].y - pair[1].y)
            .collect(),
    );
    let limit = TABLE_ROW_GAP * pitch;
    let holds = |index: usize, neighbour: usize| {
        (groups[neighbour].y - groups[index].y).abs() <= limit
            && table_row(&groups[index], columns).is_some()
    };
    let mut lo = start;
    while lo > 0 && holds(lo - 1, lo) {
        lo -= 1;
    }
    let mut hi = end;
    while hi < groups.len() && holds(hi, hi - 1) {
        hi += 1;
    }
    (lo, hi)
}

/// How many cell columns the rows themselves draw in, a colspan cell
/// counting for every column it covers. A page number, folio or marginal
/// note sitting alone out in the margin manufactures a lane, and once it is
/// hoisted out of the band nothing is left to fill the column it opened —
/// a two-column layout with an empty third column, not a grid.
fn populated_columns(rows: &[Vec<Cell>], columns: usize) -> usize {
    let mut filled = vec![false; columns];
    for row in rows {
        let mut column = 0usize;
        for cell in row {
            let width = cell.colspan as usize;
            if cell.line.is_some() {
                for slot in filled.iter_mut().skip(column).take(width) {
                    *slot = true;
                }
            }
            column += width;
        }
    }
    filled.iter().filter(|slot| **slot).count()
}

/// One line group as an assembled line, for the passes that read prose.
fn assembled(group: &Group) -> Assembled {
    assemble_line(group.y, group.size, &group.spans)
}

/// The x ranges the lanes leave between them, left to right: the band's cell
/// columns. The outer two run to the band's own horizontal extent.
fn cell_columns(spans: &[&TextSpan], lanes: &[std::ops::Range<f32>]) -> Vec<std::ops::Range<f32>> {
    let (lo, hi) = x_bounds(spans);
    let mut columns = Vec::with_capacity(lanes.len() + 1);
    let mut start = lo;
    for lane in lanes {
        columns.push(start..lane.start);
        start = lane.end;
    }
    columns.push(start..hi);
    columns
}

/// True when no step between neighbouring row baselines, top of page first,
/// exceeds [`TABLE_ROW_GAP`] times the median — what separates one grid from
/// two blocks that happen to share columns. Only populated rows are measured,
/// so a wrapped cell standing alone in a hole cannot halve it into two steps
/// small enough to pass.
fn even_rows(baselines: &[f32]) -> bool {
    let steps: Vec<f32> = baselines.windows(2).map(|pair| pair[0] - pair[1]).collect();
    if steps.is_empty() {
        return true;
    }
    let limit = TABLE_ROW_GAP * median(steps.clone());
    if limit <= 0.0 {
        return false;
    }
    steps.iter().all(|step| *step <= limit)
}

/// One row's cells, left to right, with a lineless cell for every column
/// nothing was drawn in. `None` when a span will not sit in a column: one
/// starting inside a lane, or two cells claiming the same column.
fn table_row(group: &Group, columns: &[std::ops::Range<f32>]) -> Option<Vec<Cell>> {
    let mut claimed: Vec<(usize, usize, Vec<&TextSpan>)> = Vec::new();
    for &span in &group.spans {
        let lo = span.x.min(span.end_x);
        let hi = span.x.max(span.end_x);
        let start = columns.iter().rposition(|column| column.start <= lo)?;
        if lo >= columns[start].end {
            return None;
        }
        let end = columns.iter().rposition(|column| column.start <= hi)?;
        match claimed.last_mut() {
            Some(last) if last.0 == start => {
                last.1 = last.1.max(end);
                last.2.push(span);
            }
            Some(last) if start <= last.1 => return None,
            _ => claimed.push((start, end, vec![span])),
        }
    }
    // `next` counts columns, `row` counts cells: a colspan cell is one cell
    // over several columns, so the two only agree on a grid with no merges.
    let mut row = Vec::with_capacity(columns.len());
    let mut next = 0usize;
    for (start, end, spans) in &claimed {
        for _ in next..*start {
            row.push(empty_cell());
        }
        row.push(Cell {
            line: Some(assemble_line(group.y, group.size, spans).line),
            colspan: (end - start + 1) as u8,
            rowspan: 1,
        });
        next = end + 1;
    }
    for _ in next..columns.len() {
        row.push(empty_cell());
    }
    spaced_cells(&row, group.size).then_some(row)
}

fn empty_cell() -> Cell {
    Cell {
        line: None,
        colspan: 1,
        rowspan: 1,
    }
}

/// True when neighbouring cells stand more than a word gap apart at the
/// row's largest size — the gap the flat flow turned into the single space
/// the [`Text`] adapter puts between cells. Below it the flow ran two cells
/// into one word, and reading them as cells would change what the page says.
fn spaced_cells(row: &[Cell], size: f32) -> bool {
    let lines: Vec<&Line> = row.iter().filter_map(|cell| cell.line.as_ref()).collect();
    lines
        .windows(2)
        .all(|pair| pair[1].x - pair[0].end_x > WORD_GAP * size)
}

/// The table's device-space box: every populated cell's line.
fn table_bbox(rows: &[Vec<Cell>]) -> BBox {
    let lines: Vec<Line> = rows
        .iter()
        .flatten()
        .filter_map(|cell| cell.line.clone())
        .collect();
    bbox(&lines)
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
///
/// Lanes are not carried out: a table is looked for inside a segment, over
/// its own rows, because a page's lanes are whatever every line on it leaves
/// clear together, which is nothing as soon as one line runs the full width.
fn segments(spans: &[TextSpan]) -> Vec<Vec<&TextSpan>> {
    let mut x_min = f32::INFINITY;
    let mut x_max = f32::NEG_INFINITY;
    for span in spans {
        x_min = x_min.min(span.x.min(span.end_x));
        x_max = x_max.max(span.x.max(span.end_x));
    }
    let width = x_max - x_min;
    if !width.is_finite() || width <= 0.0 {
        return vec![spans.iter().collect()];
    }
    let (separators, body): (Vec<&TextSpan>, Vec<&TextSpan>) = spans
        .iter()
        .partition(|s| (s.end_x - s.x).abs() > SEPARATOR_FRACTION * width);

    let mut occupied = [false; GUTTER_BINS];
    let scale = GUTTER_BINS as f32 / width;
    fill_bins(&mut occupied, &body, x_min, scale);
    let gaps = wide_gaps(&occupied, scale);
    let whole = || vec![spans.iter().collect::<Vec<&TextSpan>>()];

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
    // Exactly one wide interior lane is a gutter; several are the cell
    // columns of a data table, whose rows must keep reading left to right.
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
/// left side first. A column half is prose: its one gutter lane belongs to
/// the page, not to anything inside the column.
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

/// Marks every bin `spans` put ink in, rounding each span outwards so a lane
/// is never wider than the white space that drew it.
fn fill_bins(occupied: &mut [bool; GUTTER_BINS], spans: &[&TextSpan], x_min: f32, scale: f32) {
    for span in spans {
        let lo = ((span.x.min(span.end_x) - x_min) * scale).floor().max(0.0) as usize;
        let hi = ((span.x.max(span.end_x) - x_min) * scale).ceil() as usize;
        for bin in occupied.iter_mut().take(hi.min(GUTTER_BINS)).skip(lo) {
            *bin = true;
        }
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

/// The bin ranges back in device space. Bins, not device coordinates, stay
/// the currency of the gutter split: rounding a lane and cutting the page at
/// the rounded center could move a span from one column to the other.
fn lane_ranges(
    gaps: &[std::ops::Range<usize>],
    x_min: f32,
    scale: f32,
) -> Vec<std::ops::Range<f32>> {
    gaps.iter()
        .map(|gap| (x_min + gap.start as f32 / scale)..(x_min + gap.end as f32 / scale))
        .collect()
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

/// Leftmost and rightmost x of a span set.
fn x_bounds(spans: &[&TextSpan]) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for span in spans {
        lo = lo.min(span.x.min(span.end_x));
        hi = hi.max(span.x.max(span.end_x));
    }
    (lo, hi)
}

/// Horizontal extent of a span set.
fn x_span(spans: &[&TextSpan]) -> f32 {
    let (lo, hi) = x_bounds(spans);
    hi - lo
}

/// The pre-IR string builder, kept as the oracle [`layout`] is measured
/// against: it walks segments straight into a `String` with no structure in
/// between — lanes and tables included, which it knows nothing about. Any
/// divergence is a parity bug in the IR or the [`Text`] adapter.
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
        contents.push(lane_grid_content());
        contents.push(grid_with_edge_lines_content());
        contents.push(margin_number_grid_content());
        contents.push(ruled_grid_content());
        contents.push(ruled_boxed_list_content());
        contents.push(ruled_sub_word_gap_content());
        contents
    }

    /// A drawn 2x2 grid — boxed border, one interior vertical, one interior
    /// horizontal — with a word in each cell. Two cell columns leave a single
    /// lane, so only the ruled path can read it as a table.
    pub(crate) fn ruled_grid_content() -> String {
        String::from(
            "70 670 360 40 re S 250 670 m 250 710 l S 70 690 m 430 690 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 80 675 Tm (a2) Tj 1 0 0 1 260 675 Tm (b2) Tj ET",
        )
    }

    /// The corpus shape the ruled path exists for: a single-column boxed
    /// list — two verticals, five horizontals, one item per band. No lane
    /// structure at all: the lane path needs two lanes and this has none.
    pub(crate) fn ruled_boxed_list_content() -> String {
        String::from(
            "70 630 360 80 re S 70 690 m 430 690 l S 70 670 m 430 670 l S 70 650 m 430 650 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (first item) Tj \
             1 0 0 1 80 675 Tm (second item) Tj \
             1 0 0 1 80 655 Tm (third item) Tj \
             1 0 0 1 80 635 Tm (fourth item) Tj ET",
        )
    }

    /// [`ruled_grid_content`]'s lattice with one row's word split across the
    /// interior vertical at x=250 by a sub-word gap: "worl" ends at 249.4,
    /// "d" starts at 250.5. Reading the rulings as columns would cut "world"
    /// in two, so the grid must be rejected and the page must stay prose.
    pub(crate) fn ruled_sub_word_gap_content() -> String {
        String::from(
            "70 670 360 40 re S 250 670 m 250 710 l S 70 690 m 430 690 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 229.4 675 Tm (worl) Tj 1 0 0 1 250.5 675 Tm (d) Tj ET",
        )
    }

    /// Four rows of three cells on three lanes: the shape that reads as a
    /// table. The Markdown tests and the Text-parity oracle replay the same
    /// geometry, so the table path is measured against the plain flow.
    pub(crate) fn lane_grid_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// Wide enough to cross every lane, so reading it as a row would make it
    /// the grid's header — and a merged cell, which flips the whole block to
    /// the HTML dialect.
    pub(crate) const RUNNING_HEADER: &str =
        "ANFREL Pre-Election Assessment Mission Report to the Union Election Commission";

    /// Two cell columns of aligned rows and a page number alone at the right
    /// margin, below them. The number's lane opens a third cell column that
    /// no row draws in — and hoisting the number leaves the column empty, so
    /// the band is a two-column layout rather than a grid.
    pub(crate) fn margin_number_grid_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "1 0 0 1 500 600 Tm (3) Tj ET";
        content
    }

    /// [`lane_grid_content`] with the lines a real page puts around a
    /// grid: a running header above, a page number below, and one wrapped
    /// cell between two rows. All three populate a single cell; only the
    /// wrapped one is inside the grid.
    pub(crate) fn grid_with_edge_lines_content() -> String {
        format!(
            "BT /F1 10 Tf 1 0 0 1 72 760 Tm ({RUNNING_HEADER}) Tj ET {} \
             BT /F1 10 Tf 1 0 0 1 72 670 Tm (wrapped cell) Tj ET \
             BT /F1 10 Tf 1 0 0 1 72 600 Tm (24) Tj ET",
            lane_grid_content()
        )
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

    /// An already-normalized ruling, the shape extraction emits.
    fn ruling(x0: f32, y0: f32, x1: f32, y1: f32) -> Ruling {
        Ruling {
            start: pdfboss_text::Point { x: x0, y: y0 },
            end: pdfboss_text::Point { x: x1, y: y1 },
            width: 1.0,
        }
    }

    /// The four borders of a box plus one interior horizontal through its
    /// middle: the smallest lattice that qualifies as a grid.
    fn boxed_grid_rulings(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Ruling> {
        let mid = (y0 + y1) / 2.0;
        vec![
            ruling(x0, y0, x0, y1),
            ruling(x1, y0, x1, y1),
            ruling(x0, y0, x1, y0),
            ruling(x0, y1, x1, y1),
            ruling(x0, mid, x1, mid),
        ]
    }

    #[test]
    fn a_boxed_lattice_clusters_into_one_grid() {
        let grids = ruled_grids(&boxed_grid_rulings(70.0, 630.0, 430.0, 710.0));
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].xs, vec![70.0, 430.0]);
        assert_eq!(grids[0].ys, vec![630.0, 670.0, 710.0]);
        assert!(grids[0].boxed);
    }

    /// Two boxes stacked on the page share their border x positions; the gap
    /// between their extents must keep them two lattices, topmost first.
    #[test]
    fn stacked_boxes_sharing_their_x_stay_two_grids() {
        let mut rulings = boxed_grid_rulings(70.0, 600.0, 430.0, 680.0);
        rulings.extend(boxed_grid_rulings(70.0, 300.0, 430.0, 380.0));
        let grids = ruled_grids(&rulings);
        assert_eq!(grids.len(), 2);
        assert_eq!(grids[0].ys, vec![600.0, 640.0, 680.0], "topmost first");
        assert_eq!(grids[1].ys, vec![300.0, 340.0, 380.0]);
    }

    /// A plain box has only its two border horizontals — a frame, not a
    /// grid — and a lone separator line is nothing at all.
    #[test]
    fn a_plain_box_is_not_a_grid() {
        let rulings = vec![
            ruling(70.0, 630.0, 70.0, 710.0),
            ruling(430.0, 630.0, 430.0, 710.0),
            ruling(70.0, 630.0, 430.0, 630.0),
            ruling(70.0, 710.0, 430.0, 710.0),
        ];
        assert!(ruled_grids(&rulings).is_empty());
        assert!(ruled_grids(&[ruling(70.0, 400.0, 430.0, 400.0)]).is_empty());
    }

    /// A ruling that crosses nothing in the lattice — an underline elsewhere
    /// on the page — must not join it or open a phantom column.
    #[test]
    fn an_unconnected_ruling_stays_out_of_the_lattice() {
        let mut rulings = boxed_grid_rulings(70.0, 600.0, 430.0, 680.0);
        rulings.push(ruling(70.0, 100.0, 200.0, 100.0));
        let grids = ruled_grids(&rulings);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].ys, vec![600.0, 640.0, 680.0]);
    }

    /// The spans-only layout ignores drawn borders — the flat flow of the
    /// ruled fixtures is what the Text adapter must keep rendering.
    #[test]
    fn ruled_fixtures_keep_the_flat_flow() {
        assert_eq!(
            text_of(&ruled_boxed_list_content()),
            "first item\nsecond item\nthird item\nfourth item"
        );
        assert_eq!(text_of(&ruled_sub_word_gap_content()), "a1 b1\nworld");
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
