//! Spans to the layout IR: line assembly, word gaps, the two-column gutter
//! split, and the size statistics that rank headings.

use crate::ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{line_text, Output, Text};
use pdfboss_text::{ReadingOrder, Ruling, TextSpan};

/// Fraction of the device font size a horizontal gap must exceed to read
/// as a word break. The ceiling is justified LaTeX's shrunk inter-word
/// glue — 0.17 em for Times-family fonts, and a hair less under a
/// compressed text matrix — and the floor is italic corrections and
/// kerns, which stay under 0.1 em; 0.25 em sat exactly on the nominal
/// Times space width and swallowed every shrunk line's spaces.
const WORD_GAP: f32 = 0.15;
/// A span whose baseline falls outside the line's tolerance still joins the
/// line when its nominal vertical extent overlaps the line's by this
/// fraction of the smaller height: a superscript or subscript, never a
/// fraction's numerator or denominator.
const LINE_OVERLAP: f32 = 0.5;

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
/// A landscape text block splits only as a 2-up sheet — two portrait pages
/// scanned side by side — and its gutter must span at least this fraction
/// of the block's width: facing pages never touch, where a slide's or a
/// table sheet's interior lane is a cell boundary.
const TWO_UP_MIN_GUTTER: f32 = 0.05;
/// Minimum device-space gutter width, and the central band of the text
/// width the gutter's center must fall in.
const GUTTER_MIN_WIDTH: f32 = 6.0;
const GUTTER_BAND: std::ops::RangeInclusive<f32> = 0.25..=0.75;
/// Occupancy-histogram resolution for gutter detection.
const GUTTER_BINS: usize = 128;
/// The fraction of a segment's lines that may cross a lane and still leave
/// it a gutter: a running header, a page number, a heading over both
/// columns.
const GUTTER_MAX_CROSSING: f32 = 0.1;
/// A baseline rising by more than this multiple of the line size between
/// consecutive spans opens a new content-order flow: the jump from a
/// column's foot to the next column's head, never a fraction's numerator a
/// line above the text it follows.
const FLOW_STEP_UP: f32 = 2.0;
/// A baseline rising by more than this multiple of the line size also opens
/// a flow when the span lands more than [`FLOW_STEP_ASIDE`] sizes to the
/// side of the one before it: the first line of a caption or column set
/// beside the block just written, where a numerator or a stacked limit
/// stays over the text it belongs to.
const FLOW_LINE_UP: f32 = 1.0;
const FLOW_STEP_ASIDE: f32 = 2.0;
/// When more than this fraction of a page's text sits in single-line flows,
/// the stream was not written in reading order and the page is ordered by
/// geometry alone.
const FLOW_FRAGMENT_FRACTION: f32 = 0.5;

/// How far above body size a size bucket must sit before it reads as a
/// heading rather than as emphasis or a stray measurement.
const HEADING_MIN_DELTA: f32 = 1.0;
/// ATX headings stop at six `#`.
const HEADING_MAX_LEVEL: u8 = 6;
/// A wholly bold body-size line this short reads as a title; anything
/// longer is a sentence that happens to be bold.
const BOLD_HEADING_MAX_CHARS: usize = 72;
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
/// How many lines a band must hold before rows are inferred inside it — the
/// dominant band of a table drawn with column rules but no row rules, which
/// otherwise folds its whole body into one line of cells. Below it, and in
/// any band holding a minority of the claim's lines, a multi-line band is a
/// wrapped row whose lines merge as ever.
const BAND_INFER_MIN_LINES: usize = 4;
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

/// Groups spans into lines (baselines within `0.5 · size`), inserts a space
/// at horizontal gaps wider than `WORD_GAP` times the size, and joins lines
/// with `\n`, the lines in the [`ReadingOrder`] given: the content stream's
/// flows corrected by geometry, the structure tree's order as the extractor
/// settled it, or position alone. A page with a clear two-column gutter
/// reads column-major under the first and the last: full-width separators
/// split it into bands, and within each band the left column flows before
/// the right.
pub fn layout(spans: &[TextSpan], order: ReadingOrder) -> String {
    Text.render(&[page_layout(spans, order)])
}

/// The page's spans as structure, ranking heading sizes against this page
/// alone. Prefer [`document_layout`] whenever the whole document is at
/// hand: a page of nothing but large type has no body size of its own.
///
/// `order` is the order the spans are in: the extraction report's, for a
/// [`ReadingOrder::StructureTree`] request that fell back to content order
/// on a page the tree does not reach.
pub fn page_layout(spans: &[TextSpan], order: ReadingOrder) -> PageLayout {
    page_layout_with_rulings(spans, &[], order)
}

/// [`page_layout`] with the page's rulings: a lattice of drawn borders is
/// read as a table ahead of lane occupancy. With no rulings the two are the
/// same function.
pub fn page_layout_with_rulings(
    spans: &[TextSpan],
    rulings: &[Ruling],
    order: ReadingOrder,
) -> PageLayout {
    page_layout_with_stats(spans, rulings, &size_stats(&[spans]), order)
}

/// Every page's spans as structure, ranking heading sizes against the whole
/// document, so one oversized page cannot redefine what body text is. Each
/// page carries its own order, since a tagged document's untagged pages
/// come out in content order beside their tagged neighbours.
pub fn document_layout(pages: &[(Vec<TextSpan>, ReadingOrder)]) -> Vec<PageLayout> {
    let paired: Vec<(&[TextSpan], &[Ruling], ReadingOrder)> = pages
        .iter()
        .map(|(spans, order)| (spans.as_slice(), &[][..], *order))
        .collect();
    layouts_of(&paired)
}

/// [`document_layout`] with each page's rulings, so drawn grids become
/// tables document-wide. With no rulings the two are the same function.
pub fn document_layout_with_rulings(
    pages: &[(Vec<TextSpan>, Vec<Ruling>, ReadingOrder)],
) -> Vec<PageLayout> {
    let paired: Vec<(&[TextSpan], &[Ruling], ReadingOrder)> = pages
        .iter()
        .map(|(spans, rulings, order)| (spans.as_slice(), rulings.as_slice(), *order))
        .collect();
    layouts_of(&paired)
}

/// Every page's layout over shared document-wide size statistics.
fn layouts_of(pages: &[(&[TextSpan], &[Ruling], ReadingOrder)]) -> Vec<PageLayout> {
    let borrowed: Vec<&[TextSpan]> = pages.iter().map(|(spans, _, _)| *spans).collect();
    let stats = size_stats(&borrowed);
    let mut layouts: Vec<PageLayout> = pages
        .iter()
        .map(|(spans, rulings, order)| page_layout_with_stats(spans, rulings, &stats, *order))
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
/// headings and paragraph runs, in order. On a ruling-free layout the
/// classification is a partition — no line is reordered, merged away, or
/// dropped — which is what keeps the [`Text`] adapter byte-equal to
/// positional extraction. A drawn grid's bands merge into logical rows,
/// which preserves every token but reads cell-major.
fn page_layout_with_stats(
    spans: &[TextSpan],
    rulings: &[Ruling],
    stats: &SizeStats,
    order: ReadingOrder,
) -> PageLayout {
    let mut grids = ruled_grids(rulings);
    grids.extend(open_ruled_grids(spans, rulings, &grids));
    grids.sort_by(|a, b| b.ys[b.ys.len() - 1].total_cmp(&a.ys[a.ys.len() - 1]));
    let mut blocks = Vec::new();
    let parts = match order {
        ReadingOrder::Content => segments_with_grids(spans, &grids),
        _ => segments(spans, order),
    };
    for segment in parts {
        push_segment_blocks(segment, &grids, stats, order, &mut blocks);
    }
    PageLayout { blocks }
}

/// One segment's blocks. A segment no grid claims — every segment, when the
/// page has no rulings — takes the single lane attempt it always has.
/// Otherwise the segment is walked top-down: each claimed stretch becomes a
/// table and every uncovered remainder stretch gets that same lane attempt,
/// so a drawn grid and a whitespace-laned table can share a segment.
fn push_segment_blocks(
    segment: Segment<'_>,
    grids: &[RuledGrid],
    stats: &SizeStats,
    order: ReadingOrder,
    out: &mut Vec<Block>,
) {
    let groups = segment.into_groups();
    if grids.is_empty() {
        push_lane_blocks(&groups, stats, out);
        return;
    }
    let claims = grid_claims(&groups, grids);
    if claims.is_empty() {
        push_lane_blocks(&groups, stats, out);
        return;
    }
    let mut next = 0usize;
    for claim in claims {
        push_stretch(&groups[next..claim.range.start], stats, order, out);
        out.push(Block::Table {
            bbox: claim.bbox,
            rows: claim.rows,
        });
        next = claim.range.end;
    }
    push_stretch(&groups[next..], stats, order, out);
}

/// The lane path: one [`table_band`] attempt over the segment's line
/// groups, else prose. The groups are built once and feed both paths.
fn push_lane_blocks(groups: &[Group], stats: &SizeStats, out: &mut Vec<Block>) {
    let Some(band) = table_band(groups) else {
        push_blocks(groups.iter().map(assembled).collect(), stats, out);
        return;
    };
    push_blocks(band.above, stats, out);
    out.push(Block::Table {
        bbox: table_bbox(&band.rows),
        rows: band.rows,
    });
    push_blocks(band.below, stats, out);
}

/// A remainder stretch between grid claims, through the same lane attempt
/// a whole segment gets. Under the two orders that group lines by position
/// the stretch goes back to spans and is regrouped, so lines the grid's
/// rows once kept apart may join; under structure-tree order the groups
/// stay the segment's own, so the lines stay where the tree put them.
fn push_stretch(groups: &[Group], stats: &SizeStats, order: ReadingOrder, out: &mut Vec<Block>) {
    if groups.is_empty() {
        return;
    }
    if order == ReadingOrder::StructureTree {
        push_lane_blocks(groups, stats, out);
        return;
    }
    let spans: Vec<&TextSpan> = groups
        .iter()
        .flat_map(|group| group.spans.iter().copied())
        .collect();
    push_lane_blocks(&line_groups(&spans), stats, out);
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
    // A page holds a handful of distinct sizes, so a sorted vector beats a
    // map; the weight counts scalar starts, which is the character count of
    // valid UTF-8 without decoding it.
    let mut weights: Vec<(i32, usize)> = Vec::new();
    for span in pages.iter().flat_map(|page| page.iter()) {
        let bucket = half_points(span.size);
        let chars = span.text.bytes().filter(|b| (b & 0xC0) != 0x80).count();
        match weights.binary_search_by_key(&bucket, |(b, _)| *b) {
            Ok(index) => weights[index].1 += chars,
            Err(index) => weights.insert(index, (bucket, chars)),
        }
    }
    // Ties go to the smaller size: body text is what a document has most of
    // and, at equal weight, the likelier of the two to be it.
    let body = weights
        .iter()
        .min_by_key(|(bucket, weight)| (std::cmp::Reverse(*weight), *bucket))
        .map(|(bucket, _)| *bucket as f32 / 2.0);
    let Some(body) = body else {
        return SizeStats {
            body: 0.0,
            ladder: Vec::new(),
        };
    };
    let ladder: Vec<f32> = weights
        .iter()
        .rev()
        .map(|(bucket, _)| *bucket as f32 / 2.0)
        .filter(|size| *size >= body + HEADING_MIN_DELTA)
        .collect();
    SizeStats { body, ladder }
}

/// One assembled line and the size heading classification measures it by:
/// the smallest text that put a glyph on it, so a drop cap or an inline
/// formula cannot promote a body line — except a small-caps line, all
/// capitals in exactly two sizes, which measures by its capital size.
struct Assembled {
    line: Line,
    rank_size: f32,
}

/// Emits one segment's lines as blocks: a heading line closes the paragraph
/// run before it and takes any tightly-spaced continuation lines of the same
/// size with it. Classification walks the lines borrowed — stretches of
/// (line count, heading level or `None` for a run) — and only then are the
/// lines moved into their blocks, never cloned.
fn push_blocks(lines: Vec<Assembled>, stats: &SizeStats, out: &mut Vec<Block>) {
    let mut stretches: Vec<(usize, Option<u8>)> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let heading = heading_level(&lines[index], stats).map(|level| {
            let mut end = index + 1;
            while end < lines.len() && continues_heading(&lines[end - 1], &lines[end], stats, level)
            {
                end += 1;
            }
            (end, level)
        });
        let run_length = match heading {
            None => 1,
            Some((end, level)) => {
                let candidate = lines[index..end].iter().map(|a| &a.line);
                if heading_chars(candidate) <= HEADING_MAX_CHARS {
                    stretches.push((end - index, Some(level)));
                    index = end;
                    continue;
                }
                // Too long for a heading: the candidate folds into the run.
                end - index
            }
        };
        match stretches.last_mut() {
            Some((count, None)) => *count += run_length,
            _ => stretches.push((run_length, None)),
        }
        index += run_length;
    }
    let mut moved = lines.into_iter();
    let mut run: Vec<Line> = Vec::new();
    for (count, level) in stretches {
        let Some(level) = level else {
            run.extend(moved.by_ref().take(count).map(|a| a.line));
            push_run(&mut run, out);
            continue;
        };
        let heading: Vec<Line> = moved.by_ref().take(count).map(|a| a.line).collect();
        let bbox = bbox(&heading);
        out.push(Block::Heading {
            level,
            lines: heading,
            bbox,
        });
    }
}

/// A candidate heading's text length, counted as the Markdown adapter joins
/// its lines: one space between them, ends trimmed.
fn heading_chars<'l>(lines: impl Iterator<Item = &'l Line>) -> usize {
    lines
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
    if let Some(level) = stats.level(line.rank_size) {
        return Some(level);
    }
    if !stats.is_body(line.rank_size) {
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
    if half_points(prev.rank_size) != half_points(next.rank_size) {
        return false;
    }
    prev.line.y - next.line.y <= HEADING_MERGE_STEP * next.line.size
}

/// True for a short, wholly bold line that does not end like a sentence —
/// the run-in heading of a document that sets its headings in body size.
fn is_bold_title(line: &Line) -> bool {
    // Whitespace-only inlines carry no visible weight: a regular-face space
    // between bold words does not stop the line being a bold title.
    let mut visible = line
        .inlines
        .iter()
        .filter(|inline| !inline.text.trim().is_empty());
    let mut any = false;
    for inline in visible.by_ref() {
        any = true;
        if !inline.bold {
            return false;
        }
    }
    if !any {
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
    if lines.is_empty() {
        return;
    }
    let markers: Vec<Option<(Marker, usize)>> = lines
        .iter()
        .map(|line| list_marker(&line_text(line)))
        .collect();
    // Each list found, with the prose line count standing before it; the
    // lines only move into their blocks once the whole run is walked.
    let mut lists: Vec<(usize, Vec<ListRunItem>)> = Vec::new();
    let mut prose_count = 0usize;
    let mut index = 0;
    while index < lines.len() {
        let Some(items) = list_run(&lines[index..], &markers[index..]) else {
            prose_count += 1;
            index += 1;
            continue;
        };
        index += items.iter().map(|(_, _, count)| count).sum::<usize>();
        lists.push((prose_count, items));
        prose_count = 0;
    }
    let mut moved = lines.into_iter();
    let mut prose: Vec<Line> = Vec::new();
    for (count, items) in lists {
        prose.extend(moved.by_ref().take(count));
        push_paragraphs(&mut prose, out);
        let items: Vec<ListItem> = items
            .into_iter()
            .map(|(marker, marker_len, count)| ListItem {
                marker,
                marker_len,
                lines: moved.by_ref().take(count).collect(),
            })
            .collect();
        out.push(Block::List {
            bbox: bbox(items.iter().flat_map(|item| &item.lines)),
            items,
        });
    }
    prose.extend(moved);
    push_paragraphs(&mut prose, out);
}

/// One item of a list found by [`list_run`]: its marker, the marker's
/// length in characters, and how many of the run's lines the item takes.
type ListRunItem = (Marker, usize, usize);

/// The list opening at `lines[0]` as one [`ListRunItem`] per item, or `None`
/// when that line does not open one or the candidate falls short of
/// [`LIST_MIN_LINES`] — in which case the line is left for [`push_run`] to
/// fold back into prose. `markers` carries every line's [`list_marker`],
/// computed once for the whole run.
fn list_run(lines: &[Line], markers: &[Option<(Marker, usize)>]) -> Option<Vec<ListRunItem>> {
    let mut items = Vec::new();
    let mut consumed = 0usize;
    let mut index = 0;
    while index < lines.len() {
        let Some((marker, marker_len)) = markers[index].clone() else {
            break;
        };
        let item_x = lines[index].x;
        let item_size = lines[index].size;
        let opened = index;
        index += 1;
        while index < lines.len()
            && markers[index].is_none()
            && lines[index].x > item_x + LIST_CONTINUATION_INDENT * item_size
        {
            index += 1;
        }
        consumed += index - opened;
        items.push((marker, marker_len, index - opened));
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

/// Drains a paragraph run into blocks, cutting it where a baseline step
/// exceeds [`PARAGRAPH_GAP`] times the run's median step. The cuts are found
/// first, borrowed; the lines then move into their paragraphs uncloned.
fn push_paragraphs(run: &mut Vec<Line>, out: &mut Vec<Block>) {
    let lines = std::mem::take(run);
    if lines.is_empty() {
        return;
    }
    let limit = PARAGRAPH_GAP * median_step(&lines);
    let mut counts: Vec<usize> = Vec::new();
    let mut start = 0;
    for index in 1..lines.len() {
        if limit <= 0.0 || lines[index - 1].y - lines[index].y <= limit {
            continue;
        }
        counts.push(index - start);
        start = index;
    }
    counts.push(lines.len() - start);
    let mut moved = lines.into_iter();
    for count in counts {
        let paragraph: Vec<Line> = moved.by_ref().take(count).collect();
        out.push(Block::Paragraph {
            bbox: bbox(&paragraph),
            lines: paragraph,
            role: Role::Body,
        });
    }
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

/// True when `span` belongs on the line at baseline `y` and size `size`:
/// its baseline lies within `0.5 · size`, or its own vertical extent
/// overlaps the line's by at least [`LINE_OVERLAP`] of the smaller height.
/// The extents are the nominal ones a baseline and size imply, a quarter
/// size below and three quarters above, so a raised superscript or a sunk
/// subscript shares most of its height with the line while a fraction's
/// numerator, a whole line up, shares little.
fn same_line(y: f32, size: f32, span: &TextSpan) -> bool {
    if (y - span.y).abs() <= 0.5 * size.max(span.size) {
        return true;
    }
    let line_extent = (y - 0.25 * size, y + 0.75 * size);
    let span_extent = (span.y - 0.25 * span.size, span.y + 0.75 * span.size);
    let overlap = line_extent.1.min(span_extent.1) - line_extent.0.max(span_extent.0);
    overlap >= LINE_OVERLAP * size.min(span.size)
}

/// One reading-order segment's spans grouped into lines (see
/// [`same_line`]), top of page first, spans left to right inside each.
/// Two passes: the first assigns every span its group — each span tests
/// against the group's size as it stood when the span arrived — and counts,
/// the second fills exact-sized span lists, so no list grows push by push.
///
/// A span with exactly the baseline and size of the span before it goes
/// where that one went without scanning, provided that line's baseline
/// still lies within half a size of it: every earlier group rejected the
/// same coordinates a moment ago and has not changed since, and the
/// baseline test only loosens as a line's size grows. Consecutive words of
/// a typeset line share both values, so the scan runs once per line rather
/// than once per word.
fn line_groups<'s>(spans: &[&'s TextSpan]) -> Vec<Group<'s>> {
    let mut groups: Vec<Group> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    let mut homes: Vec<usize> = Vec::with_capacity(spans.len());
    let mut last: Option<(&TextSpan, usize)> = None;
    for &span in spans {
        let repeat = last.filter(|(prev, home)| {
            prev.y == span.y
                && prev.size == span.size
                && (groups[*home].y - span.y).abs() <= 0.5 * groups[*home].size.max(span.size)
        });
        let found = match repeat {
            Some((_, home)) => Some(home),
            None => groups
                .iter()
                .position(|group| same_line(group.y, group.size, span)),
        };
        last = Some((span, found.unwrap_or(groups.len())));
        match found {
            Some(index) => {
                groups[index].size = groups[index].size.max(span.size);
                counts[index] += 1;
                homes.push(index);
            }
            None => {
                homes.push(groups.len());
                groups.push(Group {
                    y: span.y,
                    size: span.size,
                    spans: Vec::new(),
                });
                counts.push(1);
            }
        }
    }
    for (group, count) in groups.iter_mut().zip(&counts) {
        group.spans.reserve_exact(*count);
    }
    for (&span, &home) in spans.iter().zip(&homes) {
        groups[home].spans.push(span);
    }
    groups.sort_by(|a, b| b.y.total_cmp(&a.y)); // top of page first
    for group in &mut groups {
        group.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    groups
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
    /// Whether the verticals are inferred from the text rather than drawn:
    /// an open-ruled table's bands are coarse — often one rule under the
    /// header and one closing rule — so its row inference runs on bands a
    /// lattice's would consider too thin.
    open: bool,
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

/// How closely two horizontal rules must agree at both ends to belong to
/// one open-ruled table.
const OPEN_RULED_ALIGN: f32 = 6.0;
/// The top rule may sit at most this many median sizes above the first
/// line, and the bottom rule at most [`OPEN_RULED_HUG_BOTTOM`] below the
/// last: a table's rules hug its text, a page's header and footer rules do
/// not.
const OPEN_RULED_HUG_TOP: f32 = 1.5;
const OPEN_RULED_HUG_BOTTOM: f32 = 1.0;
/// A bracketed region taller than this many median sizes is a page stripe,
/// not a table.
const OPEN_RULED_MAX_HEIGHT: f32 = 20.0;
/// A gap wider than this many sizes inside a line separates two columns —
/// low, because it applies only inside a region the rules already vouch
/// for, and a boundary must additionally never be crossed by any line.
const OPEN_RULED_COLUMN_GAP: f32 = 0.6;

/// Tables ruled only horizontally: stacked rules sharing one x-extent
/// bracket text whose lines share column gaps. Most printed tables rule
/// this way — a top rule, one under the header, a closing rule, no
/// verticals — and the text's own gaps stand in for the verticals a
/// lattice would have. The result is an ordinary [`RuledGrid`], so the
/// claim, banding, and row inference downstream apply unchanged.
///
/// A cluster that fails the hug or height gates splits at its largest rule
/// gap and each half tries again: two stacked tables share their x-extent,
/// and only splitting tells them apart. Rules inside a drawn lattice are
/// the lattice's own and never seed a cluster.
fn open_ruled_grids(spans: &[TextSpan], rulings: &[Ruling], taken: &[RuledGrid]) -> Vec<RuledGrid> {
    let mut horizontals: Vec<(f32, f32, f32)> = rulings
        .iter()
        .filter(|r| (r.end.x - r.start.x).abs() >= (r.end.y - r.start.y).abs())
        .map(|r| (r.start.y, r.start.x.min(r.end.x), r.start.x.max(r.end.x)))
        .filter(|(y, x0, x1)| {
            !taken.iter().any(|grid| {
                let b = grid.bbox();
                *x1 >= b.x0 - OPEN_RULED_ALIGN
                    && *x0 <= b.x1 + OPEN_RULED_ALIGN
                    && *y >= b.y0 - OPEN_RULED_ALIGN
                    && *y <= b.y1 + OPEN_RULED_ALIGN
            })
        })
        .collect();
    horizontals.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut grids: Vec<RuledGrid> = Vec::new();
    let mut used = vec![false; horizontals.len()];
    for seed in 0..horizontals.len() {
        if used[seed] {
            continue;
        }
        let (_, seed_x0, seed_x1) = horizontals[seed];
        let cluster: Vec<usize> = (seed..horizontals.len())
            .filter(|&i| {
                !used[i]
                    && (horizontals[i].1 - seed_x0).abs() <= OPEN_RULED_ALIGN
                    && (horizontals[i].2 - seed_x1).abs() <= OPEN_RULED_ALIGN
            })
            .collect();
        if cluster.len() < 2 {
            continue;
        }
        for &i in &cluster {
            used[i] = true;
        }
        let ys: Vec<f32> = cluster.iter().map(|&i| horizontals[i].0).collect();
        open_ruled_split(spans, &ys, seed_x0, seed_x1, taken, &mut grids);
    }
    grids
}

/// One rule cluster as a table candidate, splitting at the largest rule
/// gap when the gates reject it whole.
fn open_ruled_split(
    spans: &[TextSpan],
    ys: &[f32],
    x0: f32,
    x1: f32,
    taken: &[RuledGrid],
    out: &mut Vec<RuledGrid>,
) {
    if ys.len() < 2 {
        return;
    }
    if let Some(grid) = open_ruled_candidate(spans, ys, x0, x1, taken) {
        out.push(grid);
        return;
    }
    let widest = (1..ys.len())
        .max_by(|&a, &b| (ys[a] - ys[a - 1]).total_cmp(&(ys[b] - ys[b - 1])))
        .expect("two rules have a gap");
    open_ruled_split(spans, &ys[..widest], x0, x1, taken, out);
    open_ruled_split(spans, &ys[widest..], x0, x1, taken, out);
}

/// The gates and column inference for one bracketed region; `None` sends
/// the cluster to the split.
fn open_ruled_candidate(
    spans: &[TextSpan],
    ys: &[f32],
    x0: f32,
    x1: f32,
    taken: &[RuledGrid],
) -> Option<RuledGrid> {
    let (y_lo, y_hi) = (ys[0], ys[ys.len() - 1]);
    let region: Vec<&TextSpan> = spans
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .filter(|s| s.y >= y_lo && s.y < y_hi)
        .filter(|s| s.bbox.x1 >= x0 - OPEN_RULED_ALIGN && s.bbox.x0 <= x1 + OPEN_RULED_ALIGN)
        .collect();
    if region.is_empty() {
        return None;
    }
    let mut sizes: Vec<f32> = region.iter().map(|s| s.size).collect();
    sizes.sort_by(f32::total_cmp);
    let median = sizes[sizes.len() / 2];
    if (y_hi - y_lo) / median > OPEN_RULED_MAX_HEIGHT {
        return None;
    }
    let mut by_y = region.clone();
    by_y.sort_by(|a, b| b.y.total_cmp(&a.y));
    let mut lines: Vec<Vec<&TextSpan>> = Vec::new();
    for span in by_y {
        match lines.last_mut() {
            Some(line)
                if (line[0].y - span.y).abs() <= 0.5 * line[0].size.max(span.size) =>
            {
                line.push(span)
            }
            _ => lines.push(vec![span]),
        }
    }
    if lines.len() < 2 {
        return None;
    }
    let top_line = lines[0][0].y;
    let bottom_line = lines[lines.len() - 1][0].y;
    if (y_hi - top_line) / median > OPEN_RULED_HUG_TOP
        || (bottom_line - y_lo) / median > OPEN_RULED_HUG_BOTTOM
    {
        return None;
    }
    // Column boundaries: per-line gaps collected as intervals, overlapping
    // intervals intersected into one boundary — every straddling line must
    // agree on where the column break can be — and no line may cross it.
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    for line in &lines {
        let mut row: Vec<&&TextSpan> = line.iter().collect();
        row.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
        let mut cover = row[0].bbox.x1;
        for span in &row[1..] {
            if span.bbox.x0 - cover > OPEN_RULED_COLUMN_GAP * span.size {
                gaps.push((cover, span.bbox.x0));
            }
            cover = cover.max(span.bbox.x1);
        }
    }
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut boundaries: Vec<f32> = Vec::new();
    let (mut lo, mut hi) = gaps[0];
    for &(gap_lo, gap_hi) in &gaps[1..] {
        if gap_lo < hi {
            lo = lo.max(gap_lo);
            hi = hi.min(gap_hi);
            continue;
        }
        boundaries.push((lo + hi) / 2.0);
        (lo, hi) = (gap_lo, gap_hi);
    }
    boundaries.push((lo + hi) / 2.0);
    boundaries.retain(|mid| {
        !lines
            .iter()
            .flatten()
            .any(|s| s.bbox.x0 < *mid && *mid < s.bbox.x1)
    });
    if boundaries.is_empty() {
        return None;
    }
    let text_lo = region.iter().map(|s| s.bbox.x0).fold(f32::MAX, f32::min);
    let text_hi = region.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max);
    let mut xs = Vec::with_capacity(boundaries.len() + 2);
    xs.push(x0.min(text_lo) - 1.0);
    xs.extend(boundaries);
    xs.push(x1.max(text_hi) + 1.0);
    let grid = RuledGrid {
        xs,
        ys: ys.to_vec(),
        boxed: false,
        open: true,
    };
    // A candidate that reaches into a drawn lattice would steal its flows;
    // the lattice was there first.
    let own = grid.bbox();
    let overlaps_taken = taken.iter().any(|other| {
        let b = other.bbox();
        own.x1 >= b.x0 && own.x0 <= b.x1 && own.y1 >= b.y0 && own.y0 <= b.y1
    });
    if overlaps_taken {
        return None;
    }
    Some(grid)
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
///
/// The column rules often run past the outermost horizontal — a header row
/// bounded above by nothing but its verticals, drawn one row box at a time.
/// Where they reach beyond it, a synthetic boundary at their far end adds
/// that band, so the rows it holds stay rows of this grid.
fn lattice(verticals: &[&GridLine], horizontals: &[&GridLine]) -> Option<RuledGrid> {
    let mut xs = distinct_positions(verticals);
    let mut ys = distinct_positions(horizontals);
    if xs.len() < RULED_GRID_MIN_VERTICALS || ys.is_empty() {
        return None;
    }
    let (x_lo, x_hi) = (xs[0], xs[xs.len() - 1]);
    let (y_lo, y_hi) = (ys[0], ys[ys.len() - 1]);
    let boxed = covers(verticals, x_lo, y_lo, y_hi)
        && covers(verticals, x_hi, y_lo, y_hi)
        && covers(horizontals, y_lo, x_lo, x_hi)
        && covers(horizontals, y_hi, x_lo, x_hi);
    let reach_lo = verticals
        .iter()
        .map(|line| line.extent.start)
        .fold(f32::INFINITY, f32::min);
    let reach_hi = verticals
        .iter()
        .map(|line| line.extent.end)
        .fold(f32::NEG_INFINITY, f32::max);
    if reach_lo < y_lo - RULING_SNAP_TOLERANCE {
        ys.insert(0, reach_lo);
    }
    if reach_hi > y_hi + RULING_SNAP_TOLERANCE {
        ys.push(reach_hi);
    }
    // The mirror for columns: a frame that never reached the rulings (a
    // rounded or decorated border) leaves its row rules running past the
    // outermost verticals, and their reach is where its edges were.
    let across_lo = horizontals
        .iter()
        .map(|line| line.extent.start)
        .fold(f32::INFINITY, f32::min);
    let across_hi = horizontals
        .iter()
        .map(|line| line.extent.end)
        .fold(f32::NEG_INFINITY, f32::max);
    if across_lo < x_lo - RULING_SNAP_TOLERANCE {
        xs.insert(0, across_lo);
    }
    if across_hi > x_hi + RULING_SNAP_TOLERANCE {
        xs.push(across_hi);
    }
    if ys.len() < RULED_GRID_MIN_HORIZONTALS {
        return None;
    }
    Some(RuledGrid {
        xs,
        ys,
        boxed,
        open: false,
    })
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

/// One drawn grid's claim on a segment: the contiguous stretch of line
/// groups whose baselines fall inside the grid, the logical rows they merge
/// into, and the grid's drawn border box.
struct GridClaim {
    range: std::ops::Range<usize>,
    rows: Vec<Vec<Cell>>,
    bbox: BBox,
}

/// Every grid's claim on the segment's lines, disjoint and top-down. Grids
/// are tried topmost first, so of two lattices claiming the same lines the
/// higher wins; a grid that fails its gates claims nothing and its lines
/// stay available to the lane attempt. Lane-occupancy gates do not apply to
/// a claim — a drawn single-column box is a table no lane could show — but
/// every line still passes [`table_row`]'s word-gap cell gate.
fn grid_claims(groups: &[Group], grids: &[RuledGrid]) -> Vec<GridClaim> {
    let mut claims: Vec<GridClaim> = Vec::new();
    for grid in grids {
        let Some(claim) = grid_claim(groups, grid) else {
            continue;
        };
        let taken = claims
            .iter()
            .any(|held| held.range.start < claim.range.end && claim.range.start < held.range.end);
        if taken {
            continue;
        }
        claims.push(claim);
    }
    claims.sort_by_key(|claim| claim.range.start);
    claims
}

/// `groups` against one grid: the contiguous stretch of lines whose
/// baselines fall inside the grid becomes the claim, one logical row per
/// populated band — the y-range between consecutive horizontal rulings —
/// except a dominant rule-less band, whose rows [`anchored_rows`] infers.
/// The columns are the grid's, opened outward where the stretch's ink
/// overflows the outer verticals. `None` when a line will not sit in the
/// columns, when a column boundary lands inside a sub-word gap — the row
/// would split a word the flat flow wrote whole — or when the rows number
/// under [`TABLE_MIN_ROWS`], or under [`RULED_BOXED_MIN_ROWS`] for a grid
/// with all four borders drawn. Every line passes [`table_row`] before any
/// band's lines merge.
fn grid_claim(groups: &[Group], grid: &RuledGrid) -> Option<GridClaim> {
    let lo = groups.iter().position(|group| grid.holds(group.y))?;
    let inside = groups[lo..]
        .iter()
        .take_while(|group| grid.holds(group.y))
        .count();
    let hi = lo + inside;
    let columns = open_columns(&groups[lo..hi], grid);
    let mut rows = Vec::new();
    for band in groups[lo..hi].chunk_by(|a, b| grid.band_of(a.y) == grid.band_of(b.y)) {
        let mut lines = Vec::with_capacity(band.len());
        for group in band {
            lines.push(table_row(group, &columns)?);
        }
        let infer_floor = if grid.open { 2 } else { BAND_INFER_MIN_LINES };
        if lines.len() >= infer_floor && 2 * lines.len() > hi - lo {
            rows.append(&mut anchored_rows(lines, columns.len(), grid.open));
            continue;
        }
        rows.push(logical_row(lines, columns.len()));
    }
    if rows.len() < TABLE_MIN_ROWS
        && !((grid.boxed || grid.open) && rows.len() >= RULED_BOXED_MIN_ROWS)
    {
        return None;
    }
    Some(GridClaim {
        range: lo..hi,
        rows,
        bbox: grid.bbox(),
    })
}

/// The grid's columns, with an open outer column on each side the claimed
/// lines' ink overflows: many tables rule only the interior boundaries and
/// leave the first and last columns unboxed. The overflow may reach at most
/// the widest drawn column's width — ink farther out is a neighbouring
/// block, and without the extra column such a row fails [`table_row`]
/// exactly as it always did.
fn open_columns(groups: &[Group], grid: &RuledGrid) -> Vec<std::ops::Range<f32>> {
    // Whitespace-only spans paint nothing; their extents open no column.
    let spans: Vec<&TextSpan> = groups
        .iter()
        .flat_map(|group| group.spans.iter().copied())
        .filter(|span| !span.text.trim().is_empty())
        .collect();
    let (x_lo, x_hi) = x_bounds(&spans);
    let mut columns = grid.columns();
    let widest = columns
        .iter()
        .map(|column| column.end - column.start)
        .fold(0.0f32, f32::max);
    let first = grid.xs[0];
    let last = grid.xs[grid.xs.len() - 1];
    if x_lo < first - RULING_SNAP_TOLERANCE && first - x_lo <= widest {
        columns.insert(0, x_lo..first);
    }
    if x_hi > last + RULING_SNAP_TOLERANCE && x_hi - last <= widest {
        columns.push(last..x_hi);
    }
    columns
}

/// A rule-less band's lines as logical rows: a new row opens at every line
/// that populates the band's leftmost populated column — the anchor a
/// record's first line draws in a top-aligned table — and also stands like
/// a record, populating [`TABLE_MIN_ROW_CELLS`] cells. The lines between
/// openers are wrapped continuations, merged in behind their opener.
///
/// A band whose first line is no opener is not top-aligned: its records
/// center their cells vertically, or it is one wrapped record whose long
/// first column touches every line. There the anchor says nothing, and the
/// band merges whole, exactly as a wrapped row always has.
fn anchored_rows(lines: Vec<Vec<Cell>>, columns: usize, open: bool) -> Vec<Vec<Cell>> {
    let Some(anchor) =
        (0..columns).find(|column| lines.iter().any(|line| populates(line, *column)))
    else {
        return vec![logical_row(lines, columns)];
    };
    // An open-ruled table's lines are its rows: a fill-in table populates
    // only its first column, and each such line still opens a record. A
    // lattice keeps the two-cell demand, which is what stops a wrapped
    // first column splitting into a row per line.
    let cells_to_open = if open { 1 } else { TABLE_MIN_ROW_CELLS };
    let opens = |line: &Vec<Cell>| {
        populates(line, anchor)
            && line.iter().filter(|cell| cell.line.is_some()).count() >= cells_to_open
    };
    if !lines.first().is_some_and(opens) {
        return vec![logical_row(lines, columns)];
    }
    let mut rows = Vec::new();
    let mut group: Vec<Vec<Cell>> = Vec::new();
    for line in lines {
        if opens(&line) && !group.is_empty() {
            rows.push(logical_row(std::mem::take(&mut group), columns));
        }
        group.push(line);
    }
    if !group.is_empty() {
        rows.push(logical_row(group, columns));
    }
    rows
}

/// True when the row's cell covering `column` carries a line.
fn populates(row: &[Cell], column: usize) -> bool {
    let mut at = 0usize;
    for cell in row {
        let width = cell.colspan as usize;
        if at <= column && column < at + width {
            return cell.line.is_some();
        }
        at += width;
    }
    false
}

/// One band's visual lines as its logical row. Cells whose column intervals
/// overlap across the band's lines are fragments of one drawn cell — ink
/// crossing a vertical inside the band means the drawn cell is merged
/// there — so their lines merge, in reading order, into one cell over the
/// union of the columns. With no cell crossing a vertical that is plain
/// per-column merging, and a band of one line rebuilds into the same cells.
fn logical_row(lines: Vec<Vec<Cell>>, columns: usize) -> Vec<Cell> {
    let mut fragments: Vec<(std::ops::Range<usize>, Line)> = Vec::new();
    for cells in lines {
        let mut column = 0usize;
        for cell in cells {
            let width = cell.colspan as usize;
            if let Some(line) = cell.line {
                fragments.push((column..column + width, line));
            }
            column += width;
        }
    }
    let mut intervals: Vec<std::ops::Range<usize>> = fragments
        .iter()
        .map(|(interval, _)| interval.clone())
        .collect();
    intervals.sort_by_key(|interval| interval.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for interval in intervals {
        match merged.last_mut() {
            Some(last) if interval.start < last.end => last.end = last.end.max(interval.end),
            _ => merged.push(interval),
        }
    }
    let mut row = Vec::with_capacity(columns);
    let mut next = 0usize;
    for interval in merged {
        for _ in next..interval.start {
            row.push(empty_cell());
        }
        let cell_lines: Vec<&Line> = fragments
            .iter()
            .filter(|(held, _)| interval.start <= held.start && held.end <= interval.end)
            .map(|(_, line)| line)
            .collect();
        row.push(Cell {
            line: Some(merged_line(&cell_lines)),
            colspan: (interval.end - interval.start) as u8,
            rowspan: 1,
        });
        next = interval.end;
    }
    for _ in next..columns {
        row.push(empty_cell());
    }
    row
}

/// One cell's fragment lines — reading order — as the single line ground
/// truth wants for a wrapped cell: inlines concatenated with one plain
/// space at each fragment boundary. The space lands at the end of the run
/// before the boundary, the way [`push_span`] carries a word gap, and only
/// then does a same-styled boundary extend that run — the space keeps the
/// two fragments' tokens from fusing. Geometry is the fragments' union;
/// `y` stays the first fragment's.
fn merged_line(fragments: &[&Line]) -> Line {
    let mut inlines: Vec<Inline> = Vec::new();
    let mut x = f32::INFINITY;
    let mut end_x = f32::NEG_INFINITY;
    let mut size = 0.0f32;
    for (index, fragment) in fragments.iter().enumerate() {
        x = x.min(fragment.x);
        end_x = end_x.max(fragment.end_x);
        size = size.max(fragment.size);
        for (position, inline) in fragment.inlines.iter().enumerate() {
            let Some(last) = inlines.last_mut() else {
                inlines.push(inline.clone());
                continue;
            };
            if index > 0 && position == 0 {
                last.text.push(' ');
            }
            if last.bold == inline.bold && last.italic == inline.italic {
                last.text.push_str(&inline.text);
                continue;
            }
            inlines.push(inline.clone());
        }
    }
    Line {
        inlines,
        y: fragments.first().map_or(0.0, |line| line.y),
        x,
        end_x,
        size,
    }
}

/// The longest stretch of the segment's lines that reads as a grid, or `None`
/// when no stretch does and the whole segment must flow as prose.
///
/// Lanes are measured over the candidate stretch alone, never over the whole
/// segment: a page title and a paragraph of prose put ink across the width the
/// grid keeps clear, so a segment holding anything besides its table leaves no
/// lanes at all. Adding a line can only add ink, so lanes shrink as a stretch
/// grows and never come back — a stretch is grown until they fall below
/// [`TABLE_MIN_LANES`], and that is its end. A wrapped cell standing alone in
/// one column survives inside the stretch for free: it puts ink where that
/// column already held some.
fn table_band(groups: &[Group]) -> Option<TableBand> {
    for start in 0..groups.len() {
        let (end, lanes) = lane_run(groups, start);
        if end - start < TABLE_MIN_ROWS {
            continue;
        }
        if let Some(band) = grid(groups, start, end, &lanes) {
            return Some(band);
        }
    }
    None
}

/// The stretch starting at `start` that keeps at least [`TABLE_MIN_LANES`]
/// lanes, as an exclusive end and the lanes the whole stretch leaves. Ink is
/// tracked as exact intervals, not histogram bins: a column gap of a few
/// points is real table structure that bin rounding swallows.
fn lane_run(groups: &[Group], start: usize) -> (usize, Vec<std::ops::Range<f32>>) {
    let mut occupied: Vec<std::ops::Range<f32>> = Vec::new();
    let mut lanes = Vec::new();
    for (offset, group) in groups[start..].iter().enumerate() {
        let mut next = occupied.clone();
        for span in &group.spans {
            add_ink(&mut next, span.x.min(span.end_x)..span.x.max(span.end_x));
        }
        let gaps = ink_gaps(&next);
        if gaps.len() < TABLE_MIN_LANES {
            return (start + offset, lanes);
        }
        occupied = next;
        lanes = gaps;
    }
    (groups.len(), lanes)
}

/// Adds one span's extent to a sorted, disjoint interval set, merging every
/// interval it touches.
fn add_ink(occupied: &mut Vec<std::ops::Range<f32>>, ink: std::ops::Range<f32>) {
    let at = occupied.partition_point(|held| held.end < ink.start);
    let mut merged = ink;
    while at < occupied.len() && occupied[at].start <= merged.end {
        let held = occupied.remove(at);
        merged.start = merged.start.min(held.start);
        merged.end = merged.end.max(held.end);
    }
    occupied.insert(at, merged);
}

/// The gaps between consecutive ink intervals at least [`GUTTER_MIN_WIDTH`]
/// wide — interior by construction: whatever lies beyond the outermost ink
/// is margin, not lane.
fn ink_gaps(occupied: &[std::ops::Range<f32>]) -> Vec<std::ops::Range<f32>> {
    occupied
        .windows(2)
        .filter(|pair| pair[1].start - pair[0].end >= GUTTER_MIN_WIDTH)
        .map(|pair| pair[0].end..pair[1].start)
        .collect()
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
/// nothing was drawn in. `None` when a span will not sit in a column — one
/// starting inside a lane. A span starting in any column the last cell
/// already covers extends that cell: ink crossing a boundary means the cell
/// is merged there, and everything under its covered columns reads as its
/// contents.
fn table_row(group: &Group, columns: &[std::ops::Range<f32>]) -> Option<Vec<Cell>> {
    // A span only ever extends the last claim, so each claim's spans are a
    // contiguous stretch of `group.spans` — held as a range, never copied.
    let mut claimed: Vec<(usize, usize, std::ops::Range<usize>)> = Vec::new();
    for (position, &span) in group.spans.iter().enumerate() {
        // A whitespace-only span that sits in the columns claims like any
        // other, so a cell keeps its spacing; one running outside them —
        // a producer's padding past the grid's edge — paints nothing and
        // is skipped rather than disqualifying the whole row.
        let whitespace = span.text.trim().is_empty();
        let lo = span.x.min(span.end_x);
        let hi = span.x.max(span.end_x);
        let Some(start) = columns.iter().rposition(|column| column.start <= lo) else {
            if whitespace {
                continue;
            }
            return None;
        };
        if lo >= columns[start].end {
            if whitespace {
                continue;
            }
            return None;
        }
        let Some(end) = columns.iter().rposition(|column| column.start <= hi) else {
            if whitespace {
                continue;
            }
            return None;
        };
        match claimed.last_mut() {
            Some(last) if start <= last.1 => {
                last.1 = last.1.max(end);
                last.2.end = position + 1;
            }
            _ => claimed.push((start, end, position..position + 1)),
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
            line: Some(assemble_line(group.y, group.size, &group.spans[spans.clone()]).line),
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
    bbox(rows.iter().flatten().filter_map(|cell| cell.line.as_ref()))
}

/// One line from its spans in left-to-right order: a gap wider than
/// [`WORD_GAP`] times the size becomes a space, and a change of
/// `(bold, italic)` opens a new [`Inline`].
/// The rank size of a line that mixes size buckets — the rare case, priced
/// as a second pass so single-size lines never pay for it. A small-caps
/// line — all capitals, exactly two sizes — measures by its capital size:
/// the small caps are its lowercase, not a smaller text that should
/// disqualify a heading. Anything else measures by the size that carries
/// most of its characters (ties to the smaller), so a drop cap, an inline
/// formula, or a trailing ornament cannot re-rank the line either way.
fn mixed_rank_size(spans: &[&TextSpan]) -> f32 {
    let mut buckets: Vec<(i32, usize)> = Vec::new();
    let mut max_size = f32::MIN;
    let mut lowercase = false;
    for span in spans {
        if span.text.trim().is_empty() {
            continue;
        }
        max_size = max_size.max(span.size);
        lowercase = lowercase || span.text.chars().any(|c| c.is_lowercase());
        let bucket = half_points(span.size);
        let chars = span.text.bytes().filter(|b| (b & 0xC0) != 0x80).count();
        match buckets.binary_search_by_key(&bucket, |(b, _)| *b) {
            Ok(index) => buckets[index].1 += chars,
            Err(index) => buckets.insert(index, (bucket, chars)),
        }
    }
    if buckets.len() == 2 && !lowercase {
        return max_size;
    }
    buckets
        .iter()
        .min_by_key(|(bucket, chars)| (std::cmp::Reverse(*chars), *bucket))
        .map(|(bucket, _)| *bucket as f32 / 2.0)
        .unwrap_or(max_size)
}

fn assemble_line(y: f32, size: f32, spans: &[&TextSpan]) -> Assembled {
    // Most lines are one inline run; its text is sized once for every
    // span's text and a space apiece rather than grown span by span.
    let capacity = spans.iter().map(|span| span.text.len() + 1).sum();
    let mut inlines: Vec<Inline> = Vec::with_capacity(1);
    let mut prev_end: Option<f32> = None;
    let mut prev_size = 0.0f32;
    let mut first_bucket: Option<f32> = None;
    let mut mixed = false;
    for span in spans {
        let spaced = prev_end.is_some_and(|end| span.x - end > WORD_GAP * prev_size.max(span.size));
        push_span(&mut inlines, span, spaced, capacity);
        prev_end = Some(span.end_x);
        prev_size = span.size;
        // A whitespace-only span has no visible size, so it has no vote in
        // the line's size rank: a producer's stray body-size separator on a
        // heading's baseline must not fold the heading into the paragraph.
        if span.text.trim().is_empty() {
            continue;
        }
        match first_bucket {
            None => first_bucket = Some(span.size),
            Some(first) if half_points(first) != half_points(span.size) => mixed = true,
            _ => {}
        }
    }
    let rank_size = match (first_bucket, mixed) {
        (None, _) => size,
        (Some(first), false) => first,
        (Some(_), true) => mixed_rank_size(spans),
    };
    Assembled {
        line: Line {
            inlines,
            y,
            x: spans.first().map_or(0.0, |span| span.x),
            end_x: spans.last().map_or(0.0, |span| span.end_x),
            size,
        },
        rank_size,
    }
}

/// Extends the run the span continues, or opens one when its style differs.
/// A `spaced` span puts its word-gap space at the end of the run before it,
/// so the space is never lost at a style boundary.
fn push_span(inlines: &mut Vec<Inline>, span: &TextSpan, spaced: bool, capacity: usize) {
    if let Some(last) = inlines.last_mut() {
        let already_spaced =
            last.text.ends_with(char::is_whitespace) || span.text.starts_with(char::is_whitespace);
        if spaced && !already_spaced {
            last.text.push(' ');
        }
        if last.bold == span.bold && last.italic == span.italic {
            last.text.push_str(&span.text);
            return;
        }
    }
    let mut text = String::with_capacity(capacity);
    text.push_str(&span.text);
    inlines.push(Inline {
        text,
        bold: span.bold,
        italic: span.italic,
    });
}

/// The lines' device-space box. Spans carry no glyph extents, so the top is
/// the highest baseline plus the largest size — an ascender approximation.
fn bbox<'l>(lines: impl IntoIterator<Item = &'l Line>) -> BBox {
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

/// One reading-order segment: its spans, and their line groups when the
/// gutter search already built them, so the block pass need not group the
/// same spans a second time.
struct Segment<'s> {
    spans: Vec<&'s TextSpan>,
    groups: Option<Vec<Group<'s>>>,
}

impl<'s> Segment<'s> {
    fn ungrouped(spans: Vec<&'s TextSpan>) -> Segment<'s> {
        Segment {
            spans,
            groups: None,
        }
    }

    /// The segment's line groups, built now if the gutter search did not.
    fn into_groups(self) -> Vec<Group<'s>> {
        match self.groups {
            Some(groups) => groups,
            None => line_groups(&self.spans),
        }
    }
}

/// The page's spans in reading order, cut into segments, by one of three
/// builders chosen once per page: [`content_segments`],
/// [`structure_segments`] or [`geometric_segments`].
///
/// Lanes are not carried out: a table is looked for inside a segment, over
/// its own rows, because a page's lanes are whatever every line on it leaves
/// clear together, which is nothing as soon as one line runs the full width.
fn segments(spans: &[TextSpan], order: ReadingOrder) -> Vec<Segment<'_>> {
    match order {
        ReadingOrder::Content => content_segments(spans),
        ReadingOrder::StructureTree => structure_segments(spans),
        ReadingOrder::Geometric => geometric_segments(spans),
    }
}

/// Content order: the content stream's flows first, each then split at its
/// gutter when it has one.
///
/// Content order is the order the producer wrote and, in a typeset
/// document, the order it meant: a column is emitted whole before the next
/// begins. Geometry corrects the streams that write across two columns row
/// by row, and takes over entirely when content order fragments into no
/// order at all (see [`flows`]).
fn content_segments(spans: &[TextSpan]) -> Vec<Segment<'_>> {
    segments_with_grids(spans, &[])
}

/// [`content_segments`] with the page's ruled grids: flows whose boxes touch
/// the same grid merge into one segment at the earliest one's position and
/// skip the gutter split — the grid owns its region, and a table's cell text
/// arriving as several flows (or laned at its column gap) must not fragment
/// one drawn grid into a table per piece.
fn segments_with_grids<'s>(spans: &'s [TextSpan], grids: &[RuledGrid]) -> Vec<Segment<'s>> {
    let ordered = visual_flow_order(flows(spans));
    if grids.is_empty() {
        return ordered
            .into_iter()
            .flat_map(gutter_split)
            .filter(|segment| !segment.spans.is_empty())
            .collect();
    }
    let grid_of = |flow: &[&TextSpan]| -> Option<usize> {
        let x0 = flow.iter().map(|s| s.bbox.x0).fold(f32::MAX, f32::min);
        let x1 = flow.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max);
        let y0 = flow.iter().map(|s| s.bbox.y0).fold(f32::MAX, f32::min);
        let y1 = flow.iter().map(|s| s.bbox.y1).fold(f32::MIN, f32::max);
        // The grid sharing the most area with the flow, so a caption
        // brushing one grid's edge cannot steal a flow whose body sits in
        // another.
        grids
            .iter()
            .enumerate()
            .filter_map(|(index, grid)| {
                let b = grid.bbox();
                let ox = x1.min(b.x1) - x0.max(b.x0);
                let oy = y1.min(b.y1) - y0.max(b.y0);
                (ox > 0.0 && oy > 0.0).then_some((index, ox * oy))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    };
    let assignment: Vec<Option<usize>> = ordered.iter().map(|flow| grid_of(flow)).collect();
    let mut merged: Vec<Vec<&TextSpan>> = vec![Vec::new(); grids.len()];
    for (flow, assigned) in ordered.iter().zip(&assignment) {
        if let Some(grid) = assigned {
            merged[*grid].extend(flow.iter().copied());
        }
    }
    let mut emitted = vec![false; grids.len()];
    let mut out = Vec::new();
    for (flow, assigned) in ordered.into_iter().zip(assignment) {
        let Some(grid) = assigned else {
            out.extend(gutter_split(flow));
            continue;
        };
        if !emitted[grid] {
            emitted[grid] = true;
            out.push(Segment::ungrouped(std::mem::take(&mut merged[grid])));
        }
    }
    out.retain(|segment| !segment.spans.is_empty());
    out
}

/// Structure-tree order: one segment holding the spans as they came,
/// grouped into lines as they come (see [`sequential_groups`]). The
/// extractor already put them in the tree's order; nothing here moves a
/// line, so a column the tree reads whole stays whole however the page
/// looks.
fn structure_segments(spans: &[TextSpan]) -> Vec<Segment<'_>> {
    if spans.is_empty() {
        return Vec::new();
    }
    vec![Segment {
        spans: spans.iter().collect(),
        groups: Some(sequential_groups(spans)),
    }]
}

/// Geometric order: the whole page as one flow split at its gutter, lines
/// top to bottom inside each band: position alone, the order a content
/// stream written in no order at all falls back to.
fn geometric_segments(spans: &[TextSpan]) -> Vec<Segment<'_>> {
    gutter_split(spans.iter().collect())
        .into_iter()
        .filter(|segment| !segment.spans.is_empty())
        .collect()
}

/// Lines from spans in a settled order: a span joins the line before it
/// when it sits on that line (see [`same_line`]) and opens a new one
/// otherwise, so lines stay in the order their first spans came. Spans
/// sort left to right inside each line, as [`line_groups`] sorts them.
fn sequential_groups(spans: &[TextSpan]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group> = Vec::new();
    for span in spans {
        match groups.last_mut() {
            Some(line) if same_line(line.y, line.size, span) => {
                line.size = line.size.max(span.size);
                line.spans.push(span);
            }
            _ => groups.push(Group {
                y: span.y,
                size: span.size,
                spans: vec![span],
            }),
        }
    }
    for group in &mut groups {
        group.spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    }
    groups
}

/// Pages with more flows than this keep stream order outright: the pairwise
/// separation scan is quadratic, and a page fragmented into hundreds of
/// flows is not one the reorder could read better anyway.
const VISUAL_ORDER_MAX_FLOWS: usize = 256;

/// Flows shorter than this never trade places: a figure's scattered labels
/// and stray fragments read where the producer put them, exactly as
/// [`merge_sparse_neighbours`] leaves them.
const VISUAL_ORDER_MIN_CHARS: usize = 12;

/// How much of the narrower flow's width two flows must share horizontally
/// before one can read as above the other. Without this, the top of a right
/// column clears the bottom of the left column and reorders across columns —
/// a display equation splits a column into flows small enough for that.
const VISUAL_ORDER_MIN_X_OVERLAP: f32 = 0.3;

/// The char-weighted share of flow pairs the stream must have in the wrong
/// vertical order before the page is rewritten. Below it the stream's order
/// stands, chart-axis rows and all.
const VISUAL_ORDER_MIN_DISORDER: f64 = 0.05;

/// Flows fully separated vertically that share horizontal ground read top
/// to bottom; everything else — side-by-side columns, a caption beside its
/// figure — keeps the stream's order. A designed page often draws its
/// footer or a late text box first, and a viewer's reader sees
/// top-to-bottom regardless.
///
/// Kahn's algorithm over the "lies entirely above" relation, ties broken by
/// stream position, so a page already written in reading order comes out
/// unchanged, and columns — whose flows never both overlap in x and clear
/// in y — keep the order their producer wrote.
fn visual_flow_order(flows: Vec<Vec<&TextSpan>>) -> Vec<Vec<&TextSpan>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    struct Extent {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        chars: usize,
    }

    let n = flows.len();
    if n < 2 || n > VISUAL_ORDER_MAX_FLOWS {
        return flows;
    }
    let extents: Vec<Extent> = flows
        .iter()
        .map(|flow| {
            let mut extent = Extent {
                left: f32::MAX,
                right: f32::MIN,
                bottom: f32::MAX,
                top: f32::MIN,
                chars: 0,
            };
            for span in flow {
                extent.left = extent.left.min(span.bbox.x0);
                extent.right = extent.right.max(span.bbox.x1);
                extent.bottom = extent.bottom.min(span.bbox.y0);
                extent.top = extent.top.max(span.bbox.y1);
                extent.chars += span.text.bytes().filter(|b| (b & 0xC0) != 0x80).count();
            }
            extent
        })
        .collect();
    let movers = extents
        .iter()
        .filter(|e| e.chars >= VISUAL_ORDER_MIN_CHARS)
        .count();
    if movers < 2 {
        return flows;
    }
    let mut above: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut blockers = vec![0usize; n];
    for a in 0..n {
        if extents[a].chars < VISUAL_ORDER_MIN_CHARS {
            continue;
        }
        for b in 0..n {
            if a == b || extents[b].chars < VISUAL_ORDER_MIN_CHARS {
                continue;
            }
            let overlap = extents[a].right.min(extents[b].right)
                - extents[a].left.max(extents[b].left);
            let narrower = (extents[a].right - extents[a].left)
                .min(extents[b].right - extents[b].left);
            if narrower <= 0.0 || overlap < VISUAL_ORDER_MIN_X_OVERLAP * narrower {
                continue;
            }
            if extents[a].bottom > extents[b].top {
                above[a].push(b);
                blockers[b] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> = (0..n)
        .filter(|&i| blockers[i] == 0)
        .map(Reverse)
        .collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(i)) = ready.pop() {
        order.push(i);
        for &j in &above[i] {
            blockers[j] -= 1;
            if blockers[j] == 0 {
                ready.push(Reverse(j));
            }
        }
    }
    if order.len() != n {
        // Degenerate boxes can relate two flows both ways; keep the stream.
        return flows;
    }
    if order.iter().enumerate().all(|(position, &flow)| position == flow) {
        return flows;
    }
    // A page that is already essentially in reading order stays in the
    // stream's order: a couple of displaced chart-axis rows on an otherwise
    // ordered page are the producer's business, not disorder. Only a
    // substantially scrambled page — a designed title page, an infographic
    // — is worth rewriting, measured as the char-weighted share of flow
    // pairs the stream has in the wrong vertical order.
    let mut rank = vec![0usize; n];
    for (position, &flow) in order.iter().enumerate() {
        rank[flow] = position;
    }
    let mut inverted = 0.0f64;
    let mut total = 0.0f64;
    for a in 0..n {
        for b in (a + 1)..n {
            let weight = extents[a].chars.min(extents[b].chars) as f64;
            total += weight;
            if rank[a] > rank[b] {
                inverted += weight;
            }
        }
    }
    if total == 0.0 || inverted / total < VISUAL_ORDER_MIN_DISORDER {
        return flows;
    }
    let mut slots: Vec<Option<Vec<&TextSpan>>> = flows.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("each flow placed once"))
        .collect()
}

/// The content stream's flows: runs of consecutive spans whose baselines
/// never step back up by more than [`FLOW_STEP_UP`] line sizes. A typeset
/// column is one flow, and the jump from its foot to the next column's head
/// opens another; a display fraction's numerator, a line above the baseline
/// it follows, does not. Side-by-side flows too sparse to be text columns
/// merge back into one, so a table written column by column still reaches
/// the lane detector as rows. A page with more than
/// [`FLOW_FRAGMENT_FRACTION`] of its text in single-line flows was not
/// written in reading order at all, and is one flow ordered by geometry; a
/// figure's scattered labels beside a body column are a sliver of the
/// page's text and leave the column in content order.
fn flows(spans: &[TextSpan]) -> Vec<Vec<&TextSpan>> {
    let mut flows: Vec<Vec<&TextSpan>> = Vec::new();
    for span in spans {
        match flows.last_mut() {
            Some(flow) if flow.last().is_some_and(|prev| !steps_up(prev, span)) => flow.push(span),
            _ => flows.push(vec![span]),
        }
    }
    let chars = |flow: &[&TextSpan]| -> usize { flow.iter().map(|s| s.text.chars().count()).sum() };
    let fragmented: usize = flows
        .iter()
        .filter(|flow| baseline_count(flow) == 1)
        .map(|flow| chars(flow))
        .sum();
    let total: usize = flows.iter().map(|flow| chars(flow)).sum();
    if flows.len() > 1 && fragmented as f32 > FLOW_FRAGMENT_FRACTION * total as f32 {
        return vec![spans.iter().collect()];
    }
    merge_sparse_neighbours(flows)
}

/// True when `next` opens a new flow: it sits more than [`FLOW_STEP_UP`]
/// line sizes above `prev`, or a full line above it and displaced sideways
/// by more than [`FLOW_STEP_ASIDE`] sizes. A fraction's numerator is less
/// than a line up and continues where the text left off; the first line of
/// a caption set beside the one just written is a line up and far to the
/// right.
fn steps_up(prev: &TextSpan, next: &TextSpan) -> bool {
    let size = prev.size.max(next.size);
    let rise = next.y - prev.y;
    if rise > FLOW_STEP_UP * size {
        return true;
    }
    let (prev_lo, prev_hi) = (prev.x.min(prev.end_x), prev.x.max(prev.end_x));
    let aside =
        next.x > prev_hi + FLOW_STEP_ASIDE * size || next.x < prev_lo - FLOW_STEP_ASIDE * size;
    rise > FLOW_LINE_UP * size && aside
}

/// Merges a flow into the one before it when both hold at least
/// [`TABLE_MIN_ROWS`] lines yet are too sparse to be text columns, and they
/// overlap vertically: the columns of a table written column by column,
/// which read as rows only once they share a segment. Anything shorter, a
/// figure's scattered labels or a two-line caption, stays in content order
/// rather than sorting by height.
fn merge_sparse_neighbours(flows: Vec<Vec<&TextSpan>>) -> Vec<Vec<&TextSpan>> {
    let table_column =
        |flow: &[&TextSpan]| !column_shaped(flow) && baseline_count(flow) >= TABLE_MIN_ROWS;
    let mut merged: Vec<Vec<&TextSpan>> = Vec::new();
    for flow in flows {
        let Some(prev) = merged.last_mut() else {
            merged.push(flow);
            continue;
        };
        if !table_column(prev) || !table_column(&flow) || !y_overlaps(prev, &flow) {
            merged.push(flow);
            continue;
        }
        prev.extend(flow);
    }
    merged
}

/// True when the baseline ranges of two span sets overlap.
fn y_overlaps(a: &[&TextSpan], b: &[&TextSpan]) -> bool {
    let (a_lo, a_hi) = y_extent(a);
    let (b_lo, b_hi) = y_extent(b);
    a_lo <= b_hi && b_lo <= a_hi
}

/// One flow split at its gutter, when it has one.
///
/// The gutter is found by x-coverage per line: each line marks the bins its
/// spans cover, and a bin more than [`GUTTER_MAX_CROSSING`] of the lines
/// cover is occupied. The one free interior run whose center sits in the
/// middle of the text width is the gutter, and the few lines that cross it
/// — a running header, a page number, a heading over both columns — are
/// the band separators between the columns above and below them. The split
/// only happens when both sides look like real columns (enough spans,
/// enough distinct baselines, enough shared height); anything less reads
/// top to bottom as one segment.
fn gutter_split(spans: Vec<&TextSpan>) -> Vec<Segment<'_>> {
    if spans.len() < COLUMN_MIN_SPANS {
        return vec![Segment::ungrouped(spans)];
    }
    let (x_min, x_max) = x_bounds(&spans);
    let width = x_max - x_min;
    if !width.is_finite() || width <= 0.0 {
        return vec![Segment::ungrouped(spans)];
    }
    let lines = line_groups(&spans);
    match split_at_gutter(&lines, x_min, width) {
        Some(bands) => bands,
        None => vec![Segment {
            spans,
            groups: Some(lines),
        }],
    }
}

/// The bands of a flow that has a gutter, or `None` when it has none and
/// reads whole. `lines` are the flow's line groups, which the caller keeps
/// for the block pass when the flow stays whole.
fn split_at_gutter<'s>(lines: &[Group<'s>], x_min: f32, width: f32) -> Option<Vec<Segment<'s>>> {
    let scale = GUTTER_BINS as f32 / width;
    let mut coverage = [0usize; GUTTER_BINS];
    for line in lines {
        let mut covered = [false; GUTTER_BINS];
        fill_bins(&mut covered, &line.spans, x_min, scale);
        for (count, hit) in coverage.iter_mut().zip(covered) {
            *count += usize::from(hit);
        }
    }
    let allowed = (GUTTER_MAX_CROSSING * lines.len() as f32) as usize;
    let occupied: [bool; GUTTER_BINS] = std::array::from_fn(|bin| coverage[bin] > allowed);
    let gaps = wide_gaps(&occupied, scale);
    // Exactly one wide interior lane is a gutter; several are the cell
    // columns of a data table, whose rows must keep reading left to right.
    let [gutter] = gaps.as_slice() else {
        return None;
    };
    let center = (gutter.start + gutter.end) as f32 / 2.0 / GUTTER_BINS as f32;
    if !GUTTER_BAND.contains(&center) {
        return None;
    }
    let cut = x_min + (gutter.start + gutter.end) as f32 / 2.0 / scale;

    let (crossing, columns): (Vec<&Group>, Vec<&Group>) = lines.iter().partition(|line| {
        line.spans
            .iter()
            .any(|s| s.x.min(s.end_x) < cut && s.x.max(s.end_x) > cut)
    });
    let body: Vec<&TextSpan> = columns
        .iter()
        .flat_map(|line| line.spans.iter().copied())
        .collect();
    let (left, right): (Vec<&TextSpan>, Vec<&TextSpan>) =
        body.iter().partition(|s| s.x.max(s.end_x) <= cut);
    // Two-column flow lives on portrait-shaped text blocks. A block wider
    // than it is tall is a slide or a table sheet, where a lone lane is a
    // cell boundary, not a gutter — unless it is a 2-up sheet: a gutter no
    // narrower than [`TWO_UP_MIN_GUTTER`] of the width with a portrait page
    // shape on each side of it.
    let (body_lo, body_hi) = y_extent(&body);
    if body_hi - body_lo <= width {
        let gutter_width = (gutter.end - gutter.start) as f32 / scale;
        if gutter_width < TWO_UP_MIN_GUTTER * width || !portrait(&left) || !portrait(&right) {
            return None;
        }
    }
    if !column_shaped(&left) || !column_shaped(&right) {
        return None;
    }
    if x_span(&left) < COLUMN_MIN_SIDE_WIDTH * width
        || x_span(&right) < COLUMN_MIN_SIDE_WIDTH * width
    {
        return None;
    }
    let (left_lo, left_hi) = y_extent(&left);
    let (right_lo, right_hi) = y_extent(&right);
    let height = left_hi.max(right_hi) - left_lo.min(right_lo);
    if height <= 0.0
        || left_hi - left_lo < COLUMN_MIN_HEIGHT * height
        || right_hi - right_lo < COLUMN_MIN_HEIGHT * height
    {
        return None;
    }

    // Bands run top to bottom; each crossing line closes the columns above
    // it and reads between them and the columns below.
    let mut cuts: Vec<f32> = crossing.iter().map(|line| line.y).collect();
    cuts.sort_by(|a, b| b.total_cmp(a));
    cuts.dedup();
    let mut out: Vec<Segment<'s>> = Vec::new();
    let mut top = f32::INFINITY;
    for &sep_y in &cuts {
        push_band(&left, &right, top, sep_y, &mut out);
        out.push(Segment::ungrouped(
            crossing
                .iter()
                .filter(|line| line.y == sep_y)
                .flat_map(|line| line.spans.iter().copied())
                .collect(),
        ));
        top = sep_y;
    }
    push_band(&left, &right, top, f32::NEG_INFINITY, &mut out);
    Some(out)
}

/// Pushes one band's columns — the spans with baseline in `(bottom, top]` —
/// left side first. A column half is prose: its one gutter lane belongs to
/// the page, not to anything inside the column.
fn push_band<'s>(
    left: &[&'s TextSpan],
    right: &[&'s TextSpan],
    top: f32,
    bottom: f32,
    out: &mut Vec<Segment<'s>>,
) {
    for side in [left, right] {
        out.push(Segment::ungrouped(
            side.iter()
                .filter(|s| s.y <= top && s.y > bottom)
                .copied()
                .collect(),
        ));
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

/// The number of distinct baselines in a span set, rounded to whole points.
fn baseline_count(spans: &[&TextSpan]) -> usize {
    let mut baselines: Vec<i32> = spans.iter().map(|s| s.y.round() as i32).collect();
    baselines.sort_unstable();
    baselines.dedup();
    baselines.len()
}

/// True when a span set stands taller than it runs wide — the shape of one
/// page of a 2-up sheet.
fn portrait(spans: &[&TextSpan]) -> bool {
    let (lo, hi) = y_extent(spans);
    hi - lo > x_span(spans)
}

/// True when a gutter side has enough spans on enough distinct baselines
/// to be a text column rather than a stray cluster.
fn column_shaped(spans: &[&TextSpan]) -> bool {
    spans.len() >= COLUMN_MIN_SIDE_SPANS && baseline_count(spans) >= COLUMN_MIN_SIDE_LINES
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
pub(crate) fn layout_reference(spans: &[TextSpan], order: ReadingOrder) -> String {
    let mut out = String::new();
    for segment in segments(spans, order) {
        if !out.is_empty() {
            out.push('\n');
        }
        flow(&segment.spans, order, &mut out);
    }
    out
}

/// Lays one reading-order segment out into lines, appending to `out`: any
/// span joins any line it sits on and lines sort top of page first, except
/// under structure-tree order, where a span joins only the line before it
/// and lines stay as they came.
#[cfg(test)]
fn flow(spans: &[&TextSpan], order: ReadingOrder, out: &mut String) {
    struct Group<'s> {
        y: f32,
        size: f32,
        spans: Vec<&'s TextSpan>,
    }
    let sequential = order == ReadingOrder::StructureTree;
    let mut lines: Vec<Group> = Vec::new();
    for &span in spans {
        let found = if sequential {
            lines
                .last_mut()
                .filter(|line| same_line(line.y, line.size, span))
        } else {
            lines
                .iter_mut()
                .find(|line| same_line(line.y, line.size, span))
        };
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
    if !sequential {
        lines.sort_by(|a, b| b.y.total_cmp(&a.y)); // top of page first
    }
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

    /// Local prototyping rig, never run in CI: dumps per page the
    /// horizontal rulings, the detected lattice grids' boxes, and compact
    /// span records as JSONL, for offline table-detection work.
    /// PDFBOSS_TABLE_DUMP_DIR names the PDF directory,
    /// PDFBOSS_TABLE_DUMP_OUT the output file.
    #[test]
    #[ignore]
    fn table_dump() {
        use std::fmt::Write as _;
        let dir = std::env::var("PDFBOSS_TABLE_DUMP_DIR").unwrap();
        let out_path = std::env::var("PDFBOSS_TABLE_DUMP_OUT").unwrap();
        let mut out = String::new();
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(doc) = Document::load(std::fs::read(&path).unwrap()) else {
                continue;
            };
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            for index in 0..doc.page_count() {
                let Ok(page) = doc.page(index) else { continue };
                let Ok((mut spans, rulings, _)) =
                    pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page)
                else {
                    continue;
                };
                crate::retain_spans_on_page(&mut spans, &page);
                let crop = page.crop_box;
                let grids = ruled_grids(&rulings);
                write!(
                    out,
                    "{{\"doc\":{:?},\"page\":{},\"crop\":[{},{},{},{}],\"grids\":[",
                    name, index, crop.x0, crop.y0, crop.x1, crop.y1
                )
                .unwrap();
                for (i, grid) in grids.iter().enumerate() {
                    let b = grid.bbox();
                    write!(
                        out,
                        "{}[{},{},{},{}]",
                        if i > 0 { "," } else { "" },
                        b.x0, b.y0, b.x1, b.y1
                    )
                    .unwrap();
                }
                out.push_str("],\"hrules\":[");
                let mut first = true;
                for r in &rulings {
                    if (r.end.x - r.start.x).abs() < (r.end.y - r.start.y).abs() {
                        continue;
                    }
                    write!(
                        out,
                        "{}[{},{},{}]",
                        if first { "" } else { "," },
                        r.start.y,
                        r.start.x.min(r.end.x),
                        r.start.x.max(r.end.x)
                    )
                    .unwrap();
                    first = false;
                }
                out.push_str("],\"spans\":[");
                for (i, s) in spans.iter().enumerate() {
                    let head: String = s
                        .text
                        .chars()
                        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' })
                        .take(12)
                        .collect();
                    write!(
                        out,
                        "{}[{},{},{},{},{},{:?}]",
                        if i > 0 { "," } else { "" },
                        s.bbox.x0,
                        s.bbox.x1,
                        s.y,
                        s.size,
                        usize::from(!s.text.trim().is_empty()),
                        head
                    )
                    .unwrap();
                }
                out.push_str("]}\n");
            }
        }
        std::fs::write(&out_path, out).unwrap();
    }

    /// Local prototyping rig, never run in CI: prints one page's rulings
    /// and what the grid detector makes of them. PDFBOSS_PROBE_PDF names
    /// the file.
    #[test]
    #[ignore]
    fn ruling_probe() {
        let path = std::env::var("PDFBOSS_PROBE_PDF").unwrap();
        let doc = Document::load(std::fs::read(&path).unwrap()).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, rulings, report) =
            pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page).unwrap();
        println!("rulings: {} (report complete: {})", rulings.len(), report.is_complete());
        for r in rulings.iter().take(20) {
            println!(
                "  ({:7.1},{:7.1}) -> ({:7.1},{:7.1}) w={:.2}",
                r.start.x, r.start.y, r.end.x, r.end.y, r.width
            );
        }
        let grids = ruled_grids(&rulings);
        println!("grids: {}", grids.len());
        for grid in &grids {
            println!("  xs {:?} ys {} boxed {}", grid.xs, grid.ys.len(), grid.boxed);
        }
        let open = open_ruled_grids(&spans, &rulings, &grids);
        println!("open grids: {}", open.len());
        for grid in &open {
            println!("  xs {:?} ys {:?}", grid.xs, grid.ys);
        }
        for (index, segment) in segments_with_grids(&spans, &grids).into_iter().enumerate() {
            let groups = segment.into_groups();
            let claims = grid_claims(&groups, &grids);
            println!("segment {index}: {} groups, {} claims", groups.len(), claims.len());
            for grid in &grids {
                let Some(lo) = groups.iter().position(|g| grid.holds(g.y)) else {
                    continue;
                };
                let inside = groups[lo..].iter().take_while(|g| grid.holds(g.y)).count();
                let hi = lo + inside;
                let tail = groups[hi..].iter().filter(|g| grid.holds(g.y)).count();
                println!(
                    "  grid stretch {lo}..{hi} of {} ({} grid lines AFTER the stretch)",
                    groups.len(),
                    tail
                );
                let columns = open_columns(&groups[lo..hi], grid);
                for (gi, group) in groups[lo..hi].iter().enumerate() {
                    if table_row(group, &columns).is_none() {
                        let text: String = group
                            .spans
                            .iter()
                            .flat_map(|s| s.text.chars())
                            .take(60)
                            .collect();
                        println!("  table_row fails at line {}: {:?}", lo + gi, text);
                    }
                }
            }
        }
    }

    /// Local prototyping rig, never run in CI: dumps every page's flows —
    /// stream order, bbox, mass, text head — as JSONL for the reading-order
    /// policy prototype. Directories via PDFBOSS_FLOW_DUMP_DIR (PDFs) and
    /// PDFBOSS_FLOW_DUMP_OUT (output file).
    #[test]
    #[ignore]
    fn flow_dump() {
        use std::fmt::Write as _;
        let dir = std::env::var("PDFBOSS_FLOW_DUMP_DIR").unwrap();
        let out_path = std::env::var("PDFBOSS_FLOW_DUMP_OUT").unwrap();
        let mut out = String::new();
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(doc) = Document::load(std::fs::read(&path).unwrap()) else {
                continue;
            };
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            for index in 0..doc.page_count() {
                let Ok(page) = doc.page(index) else { continue };
                let Ok((mut spans, _)) = pdfboss_text::extract_spans_reporting(&doc, &page)
                else {
                    continue;
                };
                crate::retain_spans_on_page(&mut spans, &page);
                let flows = flows(&spans);
                let crop = page.crop_box;
                write!(
                    out,
                    "{{\"doc\":{:?},\"page\":{},\"crop\":[{},{},{},{}],\"flows\":[",
                    name, index, crop.x0, crop.y0, crop.x1, crop.y1
                )
                .unwrap();
                for (i, flow) in flows.iter().enumerate() {
                    let x0 = flow.iter().map(|s| s.bbox.x0).fold(f32::MAX, f32::min);
                    let x1 = flow.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max);
                    let y0 = flow.iter().map(|s| s.bbox.y0).fold(f32::MAX, f32::min);
                    let y1 = flow.iter().map(|s| s.bbox.y1).fold(f32::MIN, f32::max);
                    let chars: usize = flow.iter().map(|s| s.text.chars().count()).sum();
                    let mut head: String = flow
                        .iter()
                        .flat_map(|s| s.text.chars().chain(std::iter::once(' ')))
                        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '?' })
                        .take(48)
                        .collect();
                    head = head.trim().to_string();
                    let baselines = baseline_count(flow);
                    let start_y = flow.first().map(|s| s.y).unwrap_or(0.0);
                    write!(
                        out,
                        "{}{{\"bbox\":[{},{},{},{}],\"chars\":{},\"baselines\":{},\"start_y\":{},\"head\":{:?}}}",
                        if i > 0 { "," } else { "" },
                        x0, y0, x1, y1, chars, baselines, start_y, head
                    )
                    .unwrap();
                }
                out.push_str("]}\n");
            }
        }
        std::fs::write(&out_path, out).unwrap();
    }

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
        contents.push(two_up_content(25));
        contents.push(format!(
            "BT /F1 12 Tf 72 760 Td (A quite wide heading spanning both text columns here) Tj ET {}",
            two_column_content(25)
        ));
        contents.push(lane_grid_content());
        contents.push(narrow_gap_lane_grid_content());
        contents.push(grid_with_edge_lines_content());
        contents.push(margin_number_grid_content());
        contents.push(ruled_grid_content());
        contents.push(ruled_boxed_list_content());
        contents.push(ruled_sub_word_gap_content());
        contents.push(ruled_wrapped_band_content());
        contents.push(ruled_grid_above_lane_grid_content());
        contents.push(ruled_open_grid_content());
        contents.push(ruled_wrapped_records_content());
        contents.push(ruled_centered_record_content());
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

    /// A boxed three-column grid whose bottom band wraps its first cell over
    /// three visual lines — the shape that shattered into fragmentary rows
    /// when every visual line was its own row. Ground truth wants one
    /// logical row per band, the wrapped text joined inside the cell.
    pub(crate) fn ruled_wrapped_band_content() -> String {
        String::from(
            "70 600 390 100 re S 200 600 m 200 700 l S 330 600 m 330 700 l S \
             70 660 m 460 660 l S 70 680 m 460 680 l S \
             BT /F1 10 Tf 1 0 0 1 80 685 Tm (h1) Tj 1 0 0 1 210 685 Tm (h2) Tj \
             1 0 0 1 340 685 Tm (h3) Tj \
             1 0 0 1 80 665 Tm (m1) Tj 1 0 0 1 210 665 Tm (m2) Tj \
             1 0 0 1 340 665 Tm (m3) Tj \
             1 0 0 1 80 645 Tm (wrap one) Tj 1 0 0 1 210 645 Tm (solo) Tj \
             1 0 0 1 340 645 Tm (tail) Tj \
             1 0 0 1 80 625 Tm (wrap two) Tj 1 0 0 1 80 605 Tm (wrap three) Tj ET",
        )
    }

    /// A registration-results shape: two interior verticals running past the
    /// top horizontal, no rule between the data rows, and text overflowing
    /// both unruled outer edges. The claim must extend to the verticals'
    /// reach, open a column on each side, and infer the rule-less band's
    /// rows at its anchor column.
    pub(crate) fn ruled_open_grid_content() -> String {
        String::from(
            "150 600 m 150 712 l S 250 600 m 250 712 l S \
             70 600 m 330 600 l S 70 700 m 330 700 l S \
             BT /F1 10 Tf 1 0 0 1 80 703 Tm (name) Tj 1 0 0 1 160 703 Tm (count) Tj \
             1 0 0 1 260 703 Tm (note) Tj \
             1 0 0 1 80 685 Tm (alpha) Tj 1 0 0 1 160 685 Tm (one) Tj \
             1 0 0 1 260 685 Tm (xx) Tj \
             1 0 0 1 80 665 Tm (beta) Tj 1 0 0 1 160 665 Tm (two) Tj \
             1 0 0 1 260 665 Tm (yy) Tj \
             1 0 0 1 80 645 Tm (gamma) Tj 1 0 0 1 160 645 Tm (three) Tj \
             1 0 0 1 260 645 Tm (zz) Tj \
             1 0 0 1 80 625 Tm (delta) Tj 1 0 0 1 160 625 Tm (four) Tj \
             1 0 0 1 260 625 Tm (ww) Tj ET",
        )
    }

    /// [`ruled_open_grid_content`]'s lattice with records wrapping inside
    /// the rule-less band: the second line of each record populates only the
    /// middle column, so it must fold into the anchor line before it rather
    /// than stand as a row of its own.
    pub(crate) fn ruled_wrapped_records_content() -> String {
        String::from(
            "150 600 m 150 712 l S 250 600 m 250 712 l S \
             70 600 m 330 600 l S 70 700 m 330 700 l S \
             BT /F1 10 Tf 1 0 0 1 80 703 Tm (name) Tj 1 0 0 1 160 703 Tm (org) Tj \
             1 0 0 1 260 703 Tm (count) Tj \
             1 0 0 1 80 685 Tm (one) Tj 1 0 0 1 160 685 Tm (recordaa) Tj \
             1 0 0 1 260 685 Tm (c1) Tj \
             1 0 0 1 160 670 Tm (wrapa) Tj \
             1 0 0 1 80 650 Tm (two) Tj 1 0 0 1 160 650 Tm (recordbb) Tj \
             1 0 0 1 260 650 Tm (c2) Tj \
             1 0 0 1 160 635 Tm (wrapb) Tj ET",
        )
    }

    /// One record wrapping over the whole rule-less band, its long first
    /// column inked on every line and its other cells centered on the second
    /// line. The first line populates one cell only, so no line is an
    /// opener: the band must merge whole, one record, not shatter at its
    /// anchor column.
    pub(crate) fn ruled_centered_record_content() -> String {
        String::from(
            "150 600 m 150 712 l S 250 600 m 250 712 l S \
             70 600 m 330 600 l S 70 700 m 330 700 l S \
             BT /F1 10 Tf 1 0 0 1 80 703 Tm (name) Tj 1 0 0 1 160 703 Tm (org) Tj \
             1 0 0 1 260 703 Tm (count) Tj \
             1 0 0 1 80 685 Tm (actlinea) Tj \
             1 0 0 1 80 665 Tm (actlineb) Tj 1 0 0 1 160 665 Tm (union) Tj \
             1 0 0 1 260 665 Tm (c9) Tj \
             1 0 0 1 80 645 Tm (actlinec) Tj \
             1 0 0 1 80 625 Tm (actlined) Tj ET",
        )
    }

    /// The doc-81 shape: a small fully boxed grid above a whitespace-laned
    /// grid in one segment. The drawn grid claims only its own stretch; the
    /// laned rows below it must still become a table of their own.
    pub(crate) fn ruled_grid_above_lane_grid_content() -> String {
        let mut content = format!("{} BT /F1 10 Tf ", ruled_grid_content());
        for (row, y) in [(0, 560.0), (1, 540.0), (2, 520.0), (3, 500.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
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

    /// [`lane_grid_content`]'s shape with the second lane squeezed to eight
    /// points on a page-wide stretch: real structure, but under the ~two-bin
    /// floor a 128-bin occupancy histogram could resolve at this width. Only
    /// exact interval gaps keep it a lane.
    pub(crate) fn narrow_gap_lane_grid_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 500.0), (2, 528.0)] {
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
    /// cell between two rows, written where a typesetter writes it, right
    /// after the row it continues. All three populate a single cell; only
    /// the wrapped one is inside the grid.
    pub(crate) fn grid_with_edge_lines_content() -> String {
        let mut content = format!("BT /F1 10 Tf 1 0 0 1 72 760 Tm ({RUNNING_HEADER}) Tj ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
            if row == 1 {
                content += "1 0 0 1 72 670 Tm (wrapped cell) Tj ";
            }
        }
        content += "1 0 0 1 72 600 Tm (24) Tj ET";
        content
    }

    /// The page's spans, asserting the extraction report is complete: no
    /// test here expects to lose content.
    fn page_spans(doc: &Document, page: &pdfboss_core::Page) -> Vec<TextSpan> {
        let (spans, report) =
            pdfboss_text::extract_spans_reporting(doc, page, ReadingOrder::Content).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        spans
    }

    /// Extracted, laid-out text of a one-page document with `content` as
    /// its raw content stream (12pt /F1 with default widths of 500).
    fn text_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        layout(&page_spans(&doc, &page), ReadingOrder::Content)
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

    /// A stream that draws the page-bottom contact block first and the
    /// title above it afterwards (a designed title page): flows fully
    /// separated vertically read top to bottom, whatever order the
    /// producer wrote them in.
    #[test]
    fn separated_flows_read_top_to_bottom() {
        let text = text_of(
            "BT /F1 12 Tf 72 40 Td (Contact us at the office) Tj \
             72 720 Td (Annual Report) Tj 0 -20 Td (Prepared in June) Tj ET",
        );
        assert_eq!(text, "Annual Report\nPrepared in June\nContact us at the office");
    }

    /// Two side-by-side columns overlap vertically, so their stream order
    /// is kept: the producer wrote left before right, and a reorder that
    /// interleaved or swapped them would break column reading.
    #[test]
    fn overlapping_flows_keep_stream_order() {
        let text = text_of(
            "BT /F1 12 Tf 72 720 Td (Left head column text) Tj 0 -20 Td (Left foot column text) Tj ET \
             BT /F1 12 Tf 300 740 Td (Right head column text) Tj 0 -20 Td (Right foot column text) Tj ET",
        );
        assert_eq!(
            text,
            "Left head column text\nLeft foot column text\nRight head column text\nRight foot column text"
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

    /// Two portrait book pages scanned side by side onto one landscape
    /// sheet, a wide empty gutter between them. Each side's lines run wide
    /// enough to be a real page column, yet stand taller than they run.
    pub(crate) fn two_up_content(lines: u32) -> String {
        (0..lines)
            .flat_map(|i| {
                let y = 720 - i * 14;
                [
                    column_line(72, y, &format!("Left{i}")),
                    column_line(500, y, &format!("Right{i}")),
                ]
            })
            .collect()
    }

    /// A 2-up sheet is landscape, but its huge gutter and the portrait
    /// shape of each side still split it: each page reads whole.
    #[test]
    fn two_up_sheet_reads_page_by_page() {
        let text = text_of(&two_up_content(25));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "Left0a Left0b Left0c Left0d");
        assert_eq!(lines[24], "Left24a Left24b Left24c Left24d");
        assert_eq!(lines[25], "Right0a Right0b Right0c Right0d");
        assert_eq!(lines[49], "Right24a Right24b Right24c Right24d");
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

    /// A space glyph followed by a positioning gap is one word break, not
    /// two: the gap only says where the next word starts.
    #[test]
    fn a_space_glyph_before_a_word_gap_is_one_space() {
        let text = text_of("BT /F1 12 Tf 72 700 Td [(Hello ) -300 (world) -300 ( again)] TJ ET");
        assert_eq!(text, "Hello world again");
    }

    /// A running header set as one span per word, the middle word sitting
    /// over the gutter.
    fn running_header() -> String {
        "BT /F1 9 Tf 80 790 Td (Journal of) Tj 100 0 Td (manuscript) Tj 80 0 Td (no. 12345) Tj ET "
            .to_string()
    }

    /// A page number centered on the gutter, under both columns.
    fn page_number() -> String {
        "BT /F1 10 Tf 214 40 Td (7) Tj ET ".to_string()
    }

    /// The same two-column body as [`two_column_content`], emitted the way a
    /// typesetter writes it: the whole left column, then the whole right.
    fn column_by_column_content(lines: u32) -> String {
        let left: String = (0..lines)
            .map(|i| column_line(72, 720 - i * 14, &format!("L{i}")))
            .collect();
        let right: String = (0..lines)
            .map(|i| column_line(240, 720 - i * 14, &format!("R{i}")))
            .collect();
        left + &right
    }

    /// Text emitted column by column reads in that order even when the
    /// running header's words and the page number cross the gutter: the
    /// content stream already says which column comes first.
    #[test]
    fn column_by_column_emission_reads_in_content_order() {
        let text = text_of(&(running_header() + &column_by_column_content(25) + &page_number()));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 52);
        assert_eq!(lines[0], "Journal of manuscript no. 12345");
        assert_eq!(lines[1], "L0a L0b L0c L0d");
        assert_eq!(lines[25], "L24a L24b L24c L24d");
        assert_eq!(lines[26], "R0a R0b R0c R0d");
        assert_eq!(lines[50], "R24a R24b R24c R24d");
        assert_eq!(lines[51], "7");
    }

    /// Text emitted row by row across both columns still splits at the
    /// gutter when a header word and a page number cross it: a lane a
    /// couple of lines cross out of dozens is a gutter, and the lines that
    /// cross it are the bands between the columns.
    #[test]
    fn a_header_crossing_the_gutter_does_not_break_the_columns() {
        let text = text_of(&(running_header() + &two_column_content(25) + &page_number()));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 52);
        assert_eq!(lines[0], "Journal of manuscript no. 12345");
        assert_eq!(lines[1], "L0a L0b L0c L0d");
        assert_eq!(lines[26], "R0a R0b R0c R0d");
        assert_eq!(lines[51], "7");
    }

    /// A stream that emits its lines bottom-up is not reading order: when
    /// content order fragments into as many flows as there are lines, the
    /// page falls back to top-to-bottom geometry.
    #[test]
    fn a_bottom_up_stream_still_reads_top_to_bottom() {
        let content: String = (0..12)
            .rev()
            .map(|i| format!("BT /F1 12 Tf 72 {} Td (Line{i}) Tj ET ", 720 - i * 14))
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Line0");
        assert_eq!(lines[11], "Line11");
    }

    /// A figure's scattered labels, emitted in no vertical order after the
    /// body, open a flow apiece; they are a sliver of the page's text and
    /// do not send the body column back to geometry, where the labels would
    /// interleave with its lines.
    #[test]
    fn scattered_figure_labels_do_not_force_geometric_order() {
        let body: String = (0..12)
            .map(|i| {
                format!(
                    "BT /F1 12 Tf 72 {} Td (Body line number {i} of the column) Tj ET ",
                    720 - i * 14
                )
            })
            .collect();
        let labels: String = [600, 700, 580, 690, 566, 650, 720, 610, 640, 680, 590, 630]
            .iter()
            .enumerate()
            .map(|(i, y)| format!("BT /F1 8 Tf 420 {y} Td (t{i}) Tj ET "))
            .collect();
        let text = text_of(&(body + &labels));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Body line number 0 of the column");
        assert_eq!(lines[11], "Body line number 11 of the column");
        assert_eq!(lines[12], "t0");
        assert_eq!(lines.len(), 24);
    }

    /// Two sub-figure captions set side by side, each written whole: the
    /// step from the first caption's last line back up to the second's
    /// first line, one line up and well to the right, opens a new flow, so
    /// each caption reads whole rather than line by line across both.
    #[test]
    fn side_by_side_captions_read_one_after_the_other() {
        let content = "BT /F1 10 Tf 72 300 Td (\\(a\\) The marked triangles meet) Tj ET \
                       BT /F1 10 Tf 72 288 Td (at the center and on a side.) Tj ET \
                       BT /F1 10 Tf 300 300 Td (\\(b\\) The marked triangles meet) Tj ET \
                       BT /F1 10 Tf 300 288 Td (at the center and outside.) Tj ET ";
        let text = text_of(content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "(a) The marked triangles meet",
                "at the center and on a side.",
                "(b) The marked triangles meet",
                "at the center and outside.",
            ]
        );
    }

    /// A superscript raised past half the line size still overlaps most of
    /// the line's height, so it stays on the line rather than opening one of
    /// its own above it.
    #[test]
    fn a_raised_superscript_stays_on_its_line() {
        let content = "BT /F1 10 Tf 72 700 Td (10) Tj ET \
                       BT /F1 7 Tf 83.5 705.5 Td (9) Tj ET \
                       BT /F1 10 Tf 92 700 Td (stars) Tj ET";
        assert_eq!(text_of(content), "109 stars");
    }

    /// A display fraction's numerator sits a line above the baseline it is
    /// emitted after; that small step back up does not open a new flow, so
    /// the equation still reads top to bottom.
    #[test]
    fn a_fraction_numerator_stays_in_its_flow() {
        let content = "BT /F1 12 Tf 72 700 Td (Before the fraction) Tj ET \
                       BT /F1 12 Tf 200 708 Td (numerator) Tj ET \
                       BT /F1 12 Tf 200 692 Td (denominator) Tj ET \
                       BT /F1 12 Tf 300 700 Td (after it) Tj ET \
                       BT /F1 12 Tf 72 680 Td (Next line of prose) Tj ET ";
        let text = text_of(content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "numerator",
                "Before the fraction after it",
                "denominator",
                "Next line of prose"
            ]
        );
    }

    /// A table emitted column by column, each column too sparse to be a text
    /// column, is read by rows: the side-by-side flows merge back into one
    /// segment so its lanes are found.
    #[test]
    fn sparse_side_by_side_flows_read_as_table_rows() {
        let content: String = [72, 240, 400]
            .iter()
            .enumerate()
            .flat_map(|(column, &x)| {
                (0..4).map(move |row| {
                    format!(
                        "BT /F1 12 Tf {x} {} Td (C{column}R{row}) Tj ET ",
                        720 - row * 14
                    )
                })
            })
            .collect();
        let text = text_of(&content);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "C0R0 C1R0 C2R0");
        assert_eq!(lines[3], "C0R3 C1R3 C2R3");
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

    /// Column rules running past the outermost horizontals bound bands of
    /// their own: the lattice gains a synthetic boundary at each far end.
    #[test]
    fn vertical_reach_beyond_the_horizontals_adds_bands() {
        let rulings = vec![
            ruling(150.0, 590.0, 150.0, 712.0),
            ruling(250.0, 590.0, 250.0, 712.0),
            ruling(70.0, 600.0, 330.0, 600.0),
            ruling(70.0, 640.0, 330.0, 640.0),
            ruling(70.0, 700.0, 330.0, 700.0),
        ];
        let grids = ruled_grids(&rulings);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].ys, vec![590.0, 600.0, 640.0, 700.0, 712.0]);
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
