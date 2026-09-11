//! Spans to the layout IR: line assembly, word gaps, the two-column gutter
//! split, and the size statistics that rank headings.

use crate::ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
use crate::output::{line_text, Output, Text};
use pdfboss_text::{
    ArtifactKind, ReadingOrder, Ruling, StandardKind, StandardOwner, StandardType,
    StructureElement, TextSpan,
};

/// Fraction of the device font size a horizontal gap must exceed to read
/// as a word break. The ceiling is justified LaTeX's shrunk inter-word
/// glue — 0.17 em for Times-family fonts, and a hair less under a
/// compressed text matrix — and the floor is italic corrections and
/// kerns, which stay under 0.1 em; 0.25 em sat exactly on the nominal
/// Times space width and swallowed every shrunk line's spaces.
const WORD_GAP: f32 = 0.15;
/// The gap, in multiples of the type size, from the ink before a span
/// beyond which the span opens a cell of its own rather than continuing a
/// word or a sentence: a floating currency sign stands an em or more from
/// the label to its left, a word gap is a quarter of one.
const CELL_GAP: f32 = 1.0;
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
/// A lone top line set in the body's own type still reads as the page's
/// title when white space sets it apart: the gap under it must reach this
/// multiple of the paragraph's own line pitch.
const PAGE_TITLE_MIN_GAP: f32 = 1.5;
/// ...and the line must stay narrower than this fraction of the page's
/// text width — a full-measure line is prose, however isolated.
const PAGE_TITLE_MAX_WIDTH: f32 = 0.7;
/// Titles name, they do not explain: past this many words the line is a
/// sentence set off for some other reason.
const PAGE_TITLE_MAX_WORDS: usize = 10;
/// A baseline step beyond this multiple of a run's median step is white
/// space between paragraphs rather than leading inside one.
const PARAGRAPH_GAP: f32 = 1.8;
/// Consecutive heading lines of one size stay one heading while their
/// baseline step is within this multiple of the line size.
const HEADING_MERGE_STEP: f32 = 1.8;

/// The glyphs PDFs draw list bullets with: filled dot, hollow dot, square,
/// en dash, hyphen, asterisk.
const BULLETS: &[char] = &[
    '\u{2022}', '\u{25CF}', '\u{25E6}', '\u{25AA}', '\u{2013}', '-', '*',
];
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
/// The snap as a share of the page's body type size, where that is the
/// smaller: two rulings closer than this are one line, farther apart are
/// two rows' rules.
const RULING_SNAP_OF_SIZE: f32 = 0.6;
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
    let stats = size_stats(&[spans]);
    let mut layout = page_layout_with_stats(spans, rulings, &stats, order);
    promote_page_title(&mut layout.blocks, &stats);
    layout
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
    for layout in &mut layouts {
        promote_page_title(&mut layout.blocks, &stats);
    }
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
    let snap = ruling_snap(spans, rulings);
    let ruled = ruled_grids(rulings, snap);
    let open = open_ruled_grids(spans, rulings, &ruled);
    let topmost_first =
        |a: &RuledGrid, b: &RuledGrid| b.ys[b.ys.len() - 1].total_cmp(&a.ys[a.ys.len() - 1]);
    // Segmentation sees each stack of sections as one lattice, so a label
    // between two sections joins their segment; the claims are read
    // section by section and merge afterwards.
    let mut hulls = stack_hulls(&ruled, spans, snap);
    hulls.extend(open.iter().cloned());
    hulls.sort_by(topmost_first);
    let mut grids = ruled;
    grids.extend(open);
    grids.sort_by(topmost_first);
    let mut blocks = Vec::new();
    // Content the producer marked as pagination artifacts (running heads,
    // folios, watermarks) is laid out apart from the real content, so it
    // can take the page header and footer roles without joining a paragraph.
    let pagination: Vec<TextSpan> = spans.iter().filter(|s| is_pagination(s)).cloned().collect();
    let body: Vec<TextSpan>;
    let content: &[TextSpan] = if pagination.is_empty() {
        spans
    } else {
        body = spans
            .iter()
            .filter(|s| !is_pagination(s))
            .cloned()
            .collect();
        &body
    };
    let parts = match order {
        ReadingOrder::Content => segments_with_grids(content, &hulls),
        _ => segments(content, order),
    };
    for segment in parts {
        push_segment_blocks(segment, &grids, &hulls, snap, stats, order, &mut blocks);
    }
    demote_heading_runs(&mut blocks);
    demote_contents_entries(&mut blocks);
    if !pagination.is_empty() {
        attach_pagination_artifacts(spans, &pagination, order, &mut blocks);
    }
    PageLayout { blocks }
}

/// Whether a span sits inside an `/Artifact` sequence of type `Pagination`.
fn is_pagination(span: &TextSpan) -> bool {
    span.artifact
        .as_ref()
        .is_some_and(|a| a.kind == ArtifactKind::Pagination)
}

/// Pagination artifacts take the page header and footer roles on the
/// producer's word, without the repetition across pages the heuristic needs:
/// their lines above the midline of the page's text lead the page as
/// `PageHeader`, the rest close it as `PageFooter`.
///
/// Covers ISO 32000-1 §14.8.2.2.
fn attach_pagination_artifacts(
    all: &[TextSpan],
    pagination: &[TextSpan],
    order: ReadingOrder,
    blocks: &mut Vec<Block>,
) {
    let (lo, hi) = all
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), s| {
            (lo.min(s.y), hi.max(s.y))
        });
    let mid = (lo + hi) / 2.0;
    let mut head = Vec::new();
    let mut foot = Vec::new();
    for segment in segments(pagination, order) {
        for group in segment.into_groups() {
            let line = assembled(&group).line;
            if line.y > mid {
                head.push(line);
            } else {
                foot.push(line);
            }
        }
    }
    if !head.is_empty() {
        blocks.insert(
            0,
            Block::Paragraph {
                bbox: bbox(&head),
                lines: head,
                role: Role::PageHeader,
            },
        );
    }
    if !foot.is_empty() {
        blocks.push(Block::Paragraph {
            bbox: bbox(&foot),
            lines: foot,
            role: Role::PageFooter,
        });
    }
}

/// A page whose first heading announces a table of contents keeps that one
/// heading; every heading after it on the page is an entry, however large
/// it is set.
fn demote_contents_entries(blocks: &mut [Block]) {
    let mut headings = blocks.iter_mut().filter_map(|block| match block {
        Block::Heading { .. } => Some(block),
        _ => None,
    });
    let Some(Block::Heading { lines, .. }) = headings.next() else {
        return;
    };
    let title: String = lines.iter().map(line_text).collect::<Vec<_>>().join(" ");
    let title = title.trim().to_ascii_lowercase();
    if !matches!(title.as_str(), "contents" | "table of contents" | "index") {
        return;
    }
    for block in headings {
        let Block::Heading { lines, bbox, .. } = block else {
            unreachable!("the filter passes headings only");
        };
        *block = Block::Paragraph {
            lines: std::mem::take(lines),
            bbox: bbox.clone(),
            role: Role::Body,
        };
    }
}

/// A block's bounding box, whichever kind it is.
fn block_bbox(block: &Block) -> &BBox {
    match block {
        Block::Heading { bbox, .. }
        | Block::Paragraph { bbox, .. }
        | Block::List { bbox, .. }
        | Block::Table { bbox, .. } => bbox,
    }
}

/// Promotes an isolated top line to the page's title on a page whose type
/// announced no heading at all: a lone line in exactly the body's type,
/// standing [`PAGE_TITLE_MIN_GAP`] pitches above the paragraph under it,
/// narrower than [`PAGE_TITLE_MAX_WIDTH`] of the page's text, short,
/// starting uppercase, carrying no digit — a running header's usual tells
/// all absent. The title may open a multi-line block: a page whose stray
/// far-down lines skew the median step folds everything into one
/// paragraph, and the title is carved out of it. Runs after page roles are
/// tagged, so a repeated running header is already out of candidacy on
/// documents long enough to tag one.
fn promote_page_title(blocks: &mut Vec<Block>, stats: &SizeStats) {
    if blocks
        .iter()
        .any(|block| matches!(block, Block::Heading { .. }))
    {
        return;
    }
    let is_edge = |block: &Block| {
        matches!(
            block,
            Block::Paragraph {
                role: Role::PageHeader | Role::PageFooter,
                ..
            }
        )
    };
    let Some(start) = blocks.iter().position(|block| !is_edge(block)) else {
        return;
    };
    let Block::Paragraph {
        lines,
        role: Role::Body,
        ..
    } = &blocks[start]
    else {
        return;
    };
    let Some(title) = lines.first() else { return };
    // Exactly the body's type: smaller is a running header, and any larger
    // size on a page the ladder left heading-free was already refused by
    // the rank pass — a mixed line whose big glyph hides sub-body text.
    if half_points(title.size) != half_points(stats.body) {
        return;
    }
    let text = line_text(title);
    let text = text.trim();
    if !text.chars().next().is_some_and(char::is_uppercase) {
        return;
    }
    if text.chars().any(|c| c.is_ascii_digit()) || text.ends_with([',', ';', ':']) {
        return;
    }
    if text.chars().count() > BOLD_HEADING_MAX_CHARS
        || text.split_whitespace().count() > PAGE_TITLE_MAX_WORDS
    {
        return;
    }
    // The paragraph under the title: the rest of its own block, or — for a
    // lone-line block — the next body paragraph. Its first step is the
    // pitch the title's gap is measured against.
    let below: &[Line] = match lines.as_slice() {
        [_only] => {
            let Some(Block::Paragraph {
                lines: next,
                role: Role::Body,
                ..
            }) = blocks.get(start + 1)
            else {
                return;
            };
            next
        }
        [_title, rest @ ..] => rest,
        [] => return,
    };
    let [first_below, second_below, ..] = below else {
        return;
    };
    let pitch = first_below.y - second_below.y;
    if pitch <= 0.0 {
        return;
    }
    if title.y - first_below.y < PAGE_TITLE_MIN_GAP * pitch {
        return;
    }
    let text_x0 = blocks[start..]
        .iter()
        .fold(f32::INFINITY, |x0, block| x0.min(block_bbox(block).x0));
    let text_x1 = blocks[start..]
        .iter()
        .fold(f32::NEG_INFINITY, |x1, block| x1.max(block_bbox(block).x1));
    if title.end_x - title.x > PAGE_TITLE_MAX_WIDTH * (text_x1 - text_x0) {
        return;
    }
    let top = block_bbox(&blocks[start]).y1;
    if blocks[start + 1..]
        .iter()
        .any(|block| block_bbox(block).y1 > top)
    {
        return;
    }
    let Block::Paragraph { lines, .. } = &mut blocks[start] else {
        unreachable!("the candidate was matched as a paragraph");
    };
    let mut moved = std::mem::take(lines).into_iter();
    let title = moved.next().expect("the candidate had a first line");
    let remainder: Vec<Line> = moved.collect();
    let heading = Block::Heading {
        level: stats.bold_level(),
        bbox: bbox(std::slice::from_ref(&title)),
        lines: vec![title],
    };
    if remainder.is_empty() {
        blocks[start] = heading;
        return;
    }
    blocks[start] = Block::Paragraph {
        bbox: bbox(&remainder),
        lines: remainder,
        role: Role::Body,
    };
    blocks.insert(start, heading);
}

/// More consecutive same-level heading blocks than this many is a list of
/// entries — a table of contents, an index — not document structure.
const HEADING_RUN_MAX: usize = 3;

/// Demotes runs of more than [`HEADING_RUN_MAX`] same-level, same-size
/// heading blocks with nothing between them to paragraphs: a real section
/// heading has a section under it, a contents page has another entry. The
/// size is part of the key because ranks past the sixth clamp to one level
/// while staying visibly distinct.
fn demote_heading_runs(blocks: &mut [Block]) {
    let level_of = |block: &Block| match block {
        Block::Heading { level, lines, .. } => Some((
            *level,
            half_points(lines.first().map_or(0.0, |line| line.size)),
        )),
        _ => None,
    };
    let mut index = 0;
    while index < blocks.len() {
        let Some(level) = level_of(&blocks[index]) else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < blocks.len() && level_of(&blocks[end]) == Some(level) {
            end += 1;
        }
        if end - index > HEADING_RUN_MAX {
            for block in &mut blocks[index..end] {
                let Block::Heading { lines, bbox, .. } = block else {
                    unreachable!("the run holds headings only");
                };
                *block = Block::Paragraph {
                    lines: std::mem::take(lines),
                    bbox: bbox.clone(),
                    role: Role::Body,
                };
            }
        }
        index = end;
    }
}

/// One segment's blocks. A segment no grid claims — every segment, when the
/// page has no rulings — takes the single lane attempt it always has.
/// Otherwise the segment is walked top-down: each claimed stretch becomes a
/// table and every uncovered remainder stretch gets that same lane attempt,
/// so a drawn grid and a whitespace-laned table can share a segment.
fn push_segment_blocks(
    segment: Segment<'_>,
    grids: &[RuledGrid],
    hulls: &[RuledGrid],
    snap: f32,
    stats: &SizeStats,
    order: ReadingOrder,
    out: &mut Vec<Block>,
) {
    if order == ReadingOrder::StructureTree
        && segment
            .spans
            .iter()
            .any(|span| run_of(span) != Run::Untagged)
    {
        push_tagged_blocks(&segment.spans, stats, out);
        return;
    }
    let groups = segment.into_groups();
    if grids.is_empty() {
        push_lane_blocks(&groups, stats, out);
        return;
    }
    let claims = grid_claims(&groups, grids, hulls, snap);
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
    if groups.is_empty() {
        return;
    }
    let Some(mut band) = table_band(groups) else {
        push_blocks(groups.iter().map(assembled).collect(), stats, out);
        return;
    };
    let title = title_rows(groups, &band, stats);
    band.rows.drain(..title);
    band.span.start += title;
    // A row repeating the band's head opens a second table: a report sets
    // two tables one under the other, each headed by the same years, and
    // one lane run holds both. The rows from the repeated head on go back
    // to the attempt below, where the second table finds its own head.
    if let Some(split) = repeated_head(&band.rows) {
        let y = row_y(&band.rows[split]);
        let cut = groups[band.span.clone()]
            .iter()
            .position(|group| group.y <= y + 0.5 * group.size);
        if let Some(cut) = cut {
            band.rows.truncate(split);
            band.span.end = band.span.start + cut;
        }
    }
    // What stands below the grid gets the same attempt, as do the lines of
    // its own run the stretch trimmed off above it: a page's second table
    // is as much a table as its first, and the longest evenly pitched
    // stretch of a run is not the only grid in it. The lines above the run
    // were tried as run starts already and are prose.
    let above = band.run_start.min(band.span.start);
    push_blocks(groups[..above].iter().map(assembled).collect(), stats, out);
    push_lane_blocks(&groups[above..band.span.start], stats, out);
    out.push(Block::Table {
        bbox: table_bbox(&band.rows),
        rows: band.rows,
    });
    push_lane_blocks(&groups[band.span.end..], stats, out);
}

/// The index of the first row past the head that repeats it: two or more
/// of its cells beyond the first column spell the head's cells at the same
/// columns, and none spells anything else. The first table keeps at least
/// [`TABLE_MIN_ROWS`] rows.
fn repeated_head(rows: &[Vec<Cell>]) -> Option<usize> {
    let head = column_texts(rows.first()?);
    // The head's cells are its own: the first row below it spelling any of
    // them in the same column must spell them all, and stand under three
    // rows or more. A rate table whose every row repeats the same factors,
    // or a vehicle list whose rows share a body type and a fuel, repeats
    // data, not a head.
    for (index, row) in rows.iter().enumerate().skip(1) {
        let row = column_texts(row);
        let pairs = head.iter().zip(&row).skip(1);
        let matched = pairs.clone().filter(|(h, r)| h.is_some() && h == r).count();
        if matched == 0 {
            continue;
        }
        let repeats =
            matched >= TABLE_MIN_ROW_CELLS && pairs.clone().all(|(h, r)| r.is_none() || h == r);
        return (repeats && index >= TABLE_MIN_ROWS).then_some(index);
    }
    None
}

/// Each column's text in the row, whitespace collapsed; a spanning cell's
/// text stands at its first column and the columns it covers hold none.
fn column_texts(row: &[Cell]) -> Vec<Option<String>> {
    let mut texts = Vec::with_capacity(row.len());
    for cell in row {
        let text = cell.line.as_ref().filter(|_| inked_cell(cell)).map(|line| {
            line_text(line)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
        texts.push(text);
        for _ in 1..cell.colspan {
            texts.push(None);
        }
    }
    texts
}

/// The baseline of the row's first inked cell, or of its first cell with a
/// line when none is inked.
fn row_y(row: &[Cell]) -> f32 {
    row.iter()
        .find(|cell| inked_cell(cell))
        .or_else(|| row.iter().find(|cell| cell.line.is_some()))
        .and_then(|cell| cell.line.as_ref())
        .map_or(f32::NEG_INFINITY, |line| line.y)
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

/// What a span belongs to under structure-tree order: the outermost
/// block-level element on its path (§14.8.4.3) or, when the path holds none,
/// the innermost Caption or TOCI, the two grouping elements that hold text of
/// their own (§14.8.4.2); an illustration when a Figure, Formula or Form
/// encloses it instead (§14.8.4.5); untagged when the tree does not reach it
/// or nothing but containers do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    Block(StructureElement),
    Illustration,
    Untagged,
}

/// The run a span belongs to (see [`Run`]).
///
/// Covers ISO 32000-1 §14.8.3 and §14.8.4.5.
fn run_of(span: &TextSpan) -> Run {
    let Some(structure) = span.structure.as_ref() else {
        return Run::Untagged;
    };
    let path = &structure.path;
    let block = path
        .iter()
        .copied()
        .find(|element| element.standard_type.kind() == StandardKind::BlockLevel);
    if let Some(element) = block {
        return Run::Block(element);
    }
    let holder = path.iter().copied().rev().find(|element| {
        matches!(
            element.standard_type,
            StandardType::Caption | StandardType::TOCI
        )
    });
    if let Some(element) = holder {
        return Run::Block(element);
    }
    if path
        .iter()
        .any(|element| element.standard_type.kind() == StandardKind::Illustration)
    {
        return Run::Illustration;
    }
    Run::Untagged
}

/// Whether the span sits inside a `BlockQuote` grouping element (§14.8.4.2),
/// which makes its paragraph a block quotation. The other grouping elements
/// (Document, Part, Art, Sect, Div, TOC, Index, NonStruct, Private) hold
/// blocks rather than being one and leave their content as it is; Sect,
/// Part and Art nesting sets the level of an H heading.
///
/// Covers ISO 32000-1 §14.8.4.2.
fn in_block_quote(span: &TextSpan) -> bool {
    span.structure.as_ref().is_some_and(|structure| {
        structure
            .path
            .iter()
            .any(|element| element.standard_type == StandardType::BlockQuote)
    })
}

/// Blocks on the tree's own word: consecutive spans of one outermost
/// block-level element form one block, typed by that element. H1 to H6 and
/// H are headings, L a list, Table a table, every other block-level element
/// a paragraph, so two P elements a line apart stay two paragraphs and a
/// heading needs no size step; a Caption or TOCI (§14.8.4.2) with no
/// block-level element inside is a paragraph of its own. The text of a
/// Figure, Formula or Form (§14.8.4.5) is laid out by the heuristics on its
/// own, since the tree says nothing about what it is: adjacent illustrations
/// form one run, so a table a producer drew as a row of figures is still
/// read as a table, while none of it joins the untagged text or the tagged
/// blocks around it. Stretches of untagged spans go through the layout
/// heuristics as on an untagged page. Ruled grids are not consulted here: a
/// tagged table's rows are its TR elements. This is the clause's basic
/// layout model with the tree's order as the block progression direction:
/// block-level elements stack as blocks, inline-level elements flow inside
/// their block's lines.
///
/// Covers ISO 32000-1 §14.8.3, §14.8.4.2, §14.8.4.3 and §14.8.4.5.
fn push_tagged_blocks(spans: &[&TextSpan], stats: &SizeStats, out: &mut Vec<Block>) {
    for (run, spans) in stretches(spans, run_of) {
        match run {
            Run::Block(element) => push_tagged_block(element, spans, out),
            Run::Illustration | Run::Untagged => {
                push_lane_blocks(&sequential_groups(spans.iter().copied()), stats, out)
            }
        }
    }
}

/// `spans` cut into maximal stretches on which `key` is constant, each with
/// its key.
fn stretches<'r, 's, K: PartialEq>(
    spans: &'r [&'s TextSpan],
    key: impl Fn(&TextSpan) -> K,
) -> Vec<(K, &'r [&'s TextSpan])> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < spans.len() {
        let current = key(spans[start]);
        let mut end = start + 1;
        while end < spans.len() && key(spans[end]) == current {
            end += 1;
        }
        out.push((current, &spans[start..end]));
        start = end;
    }
    out
}

/// One tagged block from its element's type.
fn push_tagged_block(element: StructureElement, spans: &[&TextSpan], out: &mut Vec<Block>) {
    match element.standard_type {
        StandardType::H1 => push_tagged_heading(1, spans, out),
        StandardType::H2 => push_tagged_heading(2, spans, out),
        StandardType::H3 => push_tagged_heading(3, spans, out),
        StandardType::H4 => push_tagged_heading(4, spans, out),
        StandardType::H5 => push_tagged_heading(5, spans, out),
        StandardType::H6 => push_tagged_heading(6, spans, out),
        StandardType::H => push_tagged_heading(section_depth(spans[0]), spans, out),
        StandardType::L => push_tagged_list(element, spans, out),
        StandardType::Table => push_tagged_table(element, spans, out),
        _ => push_tagged_paragraph(spans, out),
    }
}

/// The level of an H heading: how deep its Part, Art and Sect ancestors
/// nest, at least one and at most six. Strongly structured documents number
/// their headings by nesting instead of naming a level (§14.8.4.3.2).
fn section_depth(span: &TextSpan) -> u8 {
    let depth = span.structure.as_ref().map_or(0, |structure| {
        structure
            .path
            .iter()
            .filter(|element| {
                matches!(
                    element.standard_type,
                    StandardType::Part | StandardType::Art | StandardType::Sect
                )
            })
            .count()
    });
    clamped_level(depth.max(1))
}

/// The lines of a run of tagged spans, in the order they came.
fn tagged_lines(spans: &[&TextSpan]) -> Vec<Line> {
    sequential_groups(spans.iter().copied())
        .iter()
        .map(|group| assembled(group).line)
        .collect()
}

fn push_tagged_heading(level: u8, spans: &[&TextSpan], out: &mut Vec<Block>) {
    let lines = tagged_lines(spans);
    if lines.is_empty() {
        return;
    }
    out.push(Block::Heading {
        level,
        bbox: bbox(&lines),
        lines,
    });
}

fn push_tagged_paragraph(spans: &[&TextSpan], out: &mut Vec<Block>) {
    let lines = tagged_lines(spans);
    if lines.is_empty() {
        return;
    }
    let role = if spans.iter().any(|span| in_block_quote(span)) {
        Role::Quote
    } else {
        Role::Body
    };
    out.push(Block::Paragraph {
        bbox: bbox(&lines),
        lines,
        role,
    });
}

/// The first element of one of `kinds` below `ancestor` on the span's path.
fn descendant(
    span: &TextSpan,
    ancestor: StructureElement,
    kinds: &[StandardType],
) -> Option<StructureElement> {
    span.structure
        .as_ref()?
        .path
        .iter()
        .skip_while(|element| **element != ancestor)
        .skip(1)
        .find(|element| kinds.contains(&element.standard_type))
        .copied()
}

/// A tagged list: its items are the LI elements below the L, each item's
/// lines the item's spans as they came, label included, and its marker the
/// Lbl's text: digits closed by `.` or `)` or by nothing are that number,
/// any other label is a bullet. An item with no Lbl (a slide deck's bullet
/// glyph inside the LBody) reads its marker off its first line as an
/// untagged page does. The marker's length is the label's on the item's
/// first line, so the Markdown adapter strips it as it strips a detected
/// one; a first line that is nothing but the marker stays in the item's
/// lines, so plain text keeps the glyph, and the Markdown adapter opens the
/// item on the line after it. A nested list's content stays inside the
/// item holding it. Spans in the L but in no LI (a caption) become a
/// paragraph where they stand, closing the list before them. A list whose
/// `ListNumbering` attribute names a numbering system (§14.8.5.5) is
/// numbered: an item by the number its label writes in that system, an
/// unlabelled item by the number after the previous item's
/// ([`numbered_marker`]).
fn push_tagged_list(list: StructureElement, spans: &[&TextSpan], out: &mut Vec<Block>) {
    let numbering = spans.first().and_then(|span| list_numbering(span, list));
    let mut items: Vec<ListItem> = Vec::new();
    let mut next: u32 = 1;
    for (item, run) in stretches(spans, |span| descendant(span, list, &[StandardType::LI])) {
        let Some(item) = item else {
            push_tagged_list_items(&mut items, out);
            push_tagged_paragraph(run, out);
            continue;
        };
        let lines = tagged_lines(run);
        let Some(first) = lines.first() else {
            continue;
        };
        let label: String = run
            .iter()
            .filter(|span| is_label(span, item))
            .map(|span| span.text.trim())
            .collect();
        let (marker, marker_len) = tagged_marker(&label, first);
        let marker = match numbering {
            Some(numbering) => numbered_marker(&label, marker, numbering, next),
            None => marker,
        };
        if let Marker::Number(n) = marker {
            next = n.saturating_add(1);
        }
        items.push(ListItem {
            marker,
            marker_len,
            lines,
        });
    }
    push_tagged_list_items(&mut items, out);
}

fn push_tagged_list_items(items: &mut Vec<ListItem>, out: &mut Vec<Block>) {
    if items.is_empty() {
        return;
    }
    let items = std::mem::take(items);
    out.push(Block::List {
        bbox: bbox(items.iter().flat_map(|item| &item.lines)),
        items,
    });
}

/// The numbering systems a list's `ListNumbering` attribute can name
/// (§14.8.5.5, Table 347); `None` and the bullet symbols `Disc`, `Circle`
/// and `Square` name none.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ListNumbering {
    Decimal,
    UpperRoman,
    LowerRoman,
    UpperAlpha,
    LowerAlpha,
}

impl ListNumbering {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "Decimal" => Some(Self::Decimal),
            "UpperRoman" => Some(Self::UpperRoman),
            "LowerRoman" => Some(Self::LowerRoman),
            "UpperAlpha" => Some(Self::UpperAlpha),
            "LowerAlpha" => Some(Self::LowerAlpha),
            _ => None,
        }
    }
}

/// A list's numbering system (§14.8.5.5): the `ListNumbering` the L
/// element's attribute objects of the standard owner `List` (§14.8.5.2)
/// give, the last one that has the key winning, else, the attribute being
/// inheritable (§14.8.5.3), the nearest ancestor's; `None` for a bullet
/// symbol, for `None` itself and for a list no element above gives one,
/// whose markers stay with the labels and lines.
///
/// Covers ISO 32000-1 §14.8.5.2, §14.8.5.3 and §14.8.5.5.
fn list_numbering(span: &TextSpan, list: StructureElement) -> Option<ListNumbering> {
    let structure = span.structure.as_ref()?;
    structure
        .path
        .iter()
        .rev()
        .skip_while(|element| **element != list)
        .find_map(|element| {
            structure
                .attributes
                .iter()
                .rev()
                .filter(|a| {
                    a.element == element.object && a.standard_owner() == Some(StandardOwner::List)
                })
                .find_map(|a| a.entries.get_name("ListNumbering"))
        })
        .and_then(|name| ListNumbering::from_name(&name.0))
}

/// An item's marker in a list with a numbering system: the number its label
/// writes in that system; for an item with no label, the number its line
/// opens with, else `next`; a label outside the system (a lettered sub-item
/// in a Decimal list) keeps the marker [`tagged_marker`] read.
///
/// Covers ISO 32000-1 §14.8.5.5.
fn numbered_marker(label: &str, marker: Marker, numbering: ListNumbering, next: u32) -> Marker {
    if let Some(n) = numbered_label(label, numbering) {
        return Marker::Number(n);
    }
    if !label.is_empty() {
        return marker;
    }
    match marker {
        Marker::Number(n) => Marker::Number(n),
        Marker::Bullet => Marker::Number(next),
    }
}

/// The number a label writes in a numbering system, the label closed by `.`
/// or `)` or by nothing: decimal digits; a letter of the system's case,
/// repeated past Z (A is 1, Z 26, AA 27); Roman numerals of the system's
/// case. `None` for a label outside the system.
///
/// Covers ISO 32000-1 §14.8.5.5.
fn numbered_label(label: &str, numbering: ListNumbering) -> Option<u32> {
    let body = label
        .strip_suffix('.')
        .or_else(|| label.strip_suffix(')'))
        .unwrap_or(label);
    if body.is_empty() {
        return None;
    }
    match numbering {
        ListNumbering::Decimal => body
            .chars()
            .all(|c| c.is_ascii_digit())
            .then(|| body.parse().ok())
            .flatten(),
        ListNumbering::UpperAlpha => alpha_label(body, true),
        ListNumbering::LowerAlpha => alpha_label(body, false),
        ListNumbering::UpperRoman => roman_label(body, true),
        ListNumbering::LowerRoman => roman_label(body, false),
    }
}

/// One letter of the given case, repeated: A is 1, Z 26, AA 27.
fn alpha_label(body: &str, upper: bool) -> Option<u32> {
    let mut chars = body.chars();
    let first = chars.next()?;
    let in_case = if upper {
        first.is_ascii_uppercase()
    } else {
        first.is_ascii_lowercase()
    };
    if !in_case || !chars.all(|c| c == first) {
        return None;
    }
    let letter = u32::from(first.to_ascii_uppercase()) - u32::from('A') + 1;
    Some((body.len() as u32 - 1) * 26 + letter)
}

/// Roman numerals of the given case, a numeral before a larger one
/// subtracted (IV is 4, XC 90).
fn roman_label(body: &str, upper: bool) -> Option<u32> {
    let mut total: i64 = 0;
    let mut previous: i64 = 0;
    for c in body.chars().rev() {
        let in_case = if upper {
            c.is_ascii_uppercase()
        } else {
            c.is_ascii_lowercase()
        };
        if !in_case {
            return None;
        }
        let value = match c.to_ascii_uppercase() {
            'I' => 1,
            'V' => 5,
            'X' => 10,
            'L' => 50,
            'C' => 100,
            'D' => 500,
            'M' => 1000,
            _ => return None,
        };
        if value < previous {
            total -= value;
        } else {
            total += value;
            previous = value;
        }
    }
    u32::try_from(total).ok().filter(|n| *n > 0)
}

/// Whether a span is the item's own label: the first Lbl or L below the
/// item on its path is a Lbl. A label inside a list nested in the item
/// belongs to that list's item, not to this one.
fn is_label(span: &TextSpan, item: StructureElement) -> bool {
    descendant(span, item, &[StandardType::Lbl, StandardType::L])
        .is_some_and(|element| element.standard_type == StandardType::Lbl)
}

/// An item's marker and the marker's length on the item's first line. With
/// a Lbl the marker comes from its text and the length is the label's on
/// the line: leading whitespace, the label and the whitespace after it, or
/// zero when the line does not open with the label. Without one the line
/// is read as [`list_marker`] reads an untagged line, and a line that is
/// nothing but a bullet glyph or a number is that marker in full.
fn tagged_marker(label: &str, first: &Line) -> (Marker, usize) {
    let text = line_text(first);
    if label.is_empty() {
        if let Some(found) = list_marker(&text) {
            return found;
        }
        let trimmed = text.trim();
        return match bare_marker(trimmed) {
            Some(marker) => (marker, text.chars().count()),
            None => (Marker::Bullet, 0),
        };
    }
    let marker = label_marker(label);
    let lead = text.chars().take_while(|c| c.is_whitespace()).count();
    let rest: String = text.chars().skip(lead).collect();
    if !rest.starts_with(label) {
        return (marker, 0);
    }
    let label_chars = label.chars().count();
    let trailing = rest
        .chars()
        .skip(label_chars)
        .take_while(|c| c.is_whitespace())
        .count();
    (marker, lead + label_chars + trailing)
}

/// The marker a line consisting of nothing else is: one glyph from
/// [`BULLETS`], or digits closed by `.` or `)` or by nothing.
fn bare_marker(text: &str) -> Option<Marker> {
    let mut chars = text.chars();
    if let (Some(first), None) = (chars.next(), chars.next()) {
        if BULLETS.contains(&first) {
            return Some(Marker::Bullet);
        }
    }
    match label_marker(text) {
        Marker::Number(n) => Some(Marker::Number(n)),
        Marker::Bullet => None,
    }
}

/// Digits, closed by `.` or `)` or by nothing, are that number; every other
/// label (a bullet glyph, a letter, a dash) is a bullet.
fn label_marker(label: &str) -> Marker {
    let digits: String = label.chars().take_while(char::is_ascii_digit).collect();
    let rest = &label[digits.len()..];
    if digits.is_empty() || !(rest.is_empty() || rest == "." || rest == ")") {
        return Marker::Bullet;
    }
    digits.parse().map_or(Marker::Bullet, Marker::Number)
}

/// A tagged table: its rows are the TR elements below the Table, wherever
/// THead, TBody and TFoot put them, and a row's cells its TH and TD
/// elements, each cell one line spanning the columns and rows its `ColSpan`
/// and `RowSpan` table attributes say (§14.8.5.7). Spans in the table but
/// in no row (a caption) become a paragraph ahead of the table.
fn push_tagged_table(table: StructureElement, spans: &[&TextSpan], out: &mut Vec<Block>) {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut aside: Vec<&TextSpan> = Vec::new();
    for (row, run) in stretches(spans, |span| descendant(span, table, &[StandardType::TR])) {
        match row {
            None => aside.extend(run),
            Some(row) => rows.push(tagged_cells(row, run)),
        }
    }
    if !aside.is_empty() {
        push_tagged_paragraph(&aside, out);
    }
    if rows.is_empty() {
        return;
    }
    out.push(Block::Table {
        bbox: table_bbox(&rows),
        rows,
    });
}

fn tagged_cells(row: StructureElement, spans: &[&TextSpan]) -> Vec<Cell> {
    stretches(spans, |span| {
        descendant(span, row, &[StandardType::TH, StandardType::TD])
    })
    .into_iter()
    .map(|(cell, run)| Cell {
        line: joined_line(tagged_lines(run)),
        colspan: table_span(run[0], cell, "ColSpan"),
        rowspan: table_span(run[0], cell, "RowSpan"),
    })
    .collect()
}

/// A cell's `ColSpan` or `RowSpan` (§14.8.5.7): the value the cell element's
/// attribute objects of the standard owner `Table` (§14.8.5.2) give, the
/// last one that has the key winning, at least 1 and at most 255; 1 for a
/// cell with no element or no such attribute.
///
/// Covers ISO 32000-1 §14.8.5.2 and §14.8.5.7.
fn table_span(span: &TextSpan, cell: Option<StructureElement>, key: &str) -> u8 {
    let Some(cell) = cell else {
        return 1;
    };
    let Some(structure) = span.structure.as_ref() else {
        return 1;
    };
    structure
        .attributes
        .iter()
        .rev()
        .filter(|a| a.element == cell.object && a.standard_owner() == Some(StandardOwner::Table))
        .find_map(|a| a.entries.get_int(key))
        .map_or(1, |n| n.clamp(1, 255) as u8)
}

/// A cell's lines as the one line a cell carries: the texts follow one
/// another with a single space between lines, the geometry the first
/// line's, widened to the widest.
fn joined_line(lines: Vec<Line>) -> Option<Line> {
    let mut lines = lines.into_iter();
    let mut joined = lines.next()?;
    for line in lines {
        let spaced = joined
            .inlines
            .last()
            .is_some_and(|inline| inline.text.ends_with(char::is_whitespace))
            || line
                .inlines
                .first()
                .is_some_and(|inline| inline.text.starts_with(char::is_whitespace));
        if !spaced {
            joined.inlines.push(Inline {
                text: " ".to_string(),
                bold: false,
                italic: false,
                code: false,
            });
        }
        joined.inlines.extend(line.inlines);
        joined.x = joined.x.min(line.x);
        joined.end_x = joined.end_x.max(line.end_x);
        joined.size = joined.size.max(line.size);
    }
    Some(joined)
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
/// Whether a span's text paints nothing: whitespace only, or empty. The
/// same answer as `text.trim().is_empty()`, found at the first inked
/// character instead of scanning both ends.
fn blank(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

fn half_points(size: f32) -> i32 {
    // Sizes are never negative, so adding a half and truncating rounds as
    // `round` would, without the library call it compiles to.
    (size * 2.0 + 0.5) as i32
}

/// The document's size statistics, weighted by characters shown: a title
/// carries a handful, body text carries thousands.
fn size_stats(pages: &[&[TextSpan]]) -> SizeStats {
    // A page holds a handful of distinct sizes, so a sorted vector beats a
    // map; the weight is the character count, which the standard library
    // counts a machine word at a time.
    let mut weights: Vec<(i32, usize)> = Vec::new();
    for span in pages.iter().flat_map(|page| page.iter()) {
        let bucket = half_points(span.size);
        let chars = span.text.chars().count();
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
/// body size — the bold-title rank. A caption's lead never ranks: a figure
/// caption set larger or bolder than body text is still a caption.
fn heading_level(line: &Assembled, stats: &SizeStats) -> Option<u8> {
    let level = match stats.level(line.rank_size) {
        Some(level) => level,
        None => {
            if !stats.is_body(line.rank_size) || !is_bold_title(&line.line) {
                return None;
            }
            stats.bold_level()
        }
    };
    if caption_lead(&line_text(&line.line)) {
        return None;
    }
    Some(level)
}

/// True when the text opens like a caption: a marker word, then a number.
fn caption_lead(text: &str) -> bool {
    let trimmed = text.trim_start();
    ["Figure", "FIGURE", "Fig.", "FIG.", "Table", "TABLE", "Tab."]
        .iter()
        .filter_map(|marker| trimmed.strip_prefix(marker))
        .any(|rest| {
            rest.trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
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
    let mut visible = line.inlines.iter().filter(|inline| !blank(&inline.text));
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
    rows: Vec<Vec<Cell>>,
    /// The segment's groups the rows came from, so the caller lays out what
    /// stands above and below.
    span: std::ops::Range<usize>,
    /// The first group of the lane run the band was cut from: the lines
    /// above it were tried as run starts and failed, so only the run's own
    /// trimmed lines get another attempt.
    run_start: usize,
}

/// A lattice of drawn rulings: the x positions of its vertical lines and the
/// y positions of its horizontal lines, ascending, joined by their crossings
/// into one connected region. Built by [`ruled_grids`].
#[derive(Clone)]
struct RuledGrid {
    xs: Vec<f32>,
    ys: Vec<f32>,
    /// The x extent the horizontal rules at each of `ys` are drawn over,
    /// one per entry: a rule the verticals' reach implied rather than one
    /// drawn spans the grid's width.
    reach: Vec<std::ops::Range<f32>>,
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

    /// True when the line's ink overlaps the drawn extent of the rules
    /// above and below its band. A chart's frame and the table beneath it
    /// weld into one lattice through a shared vertical, and the axis labels
    /// left of the frame fall in its bands with no rule over them: they are
    /// no rows of the lattice.
    fn under_rules(&self, group: &Group) -> bool {
        let inked: Vec<&TextSpan> = group
            .spans
            .iter()
            .filter(|span| !blank(&span.text))
            .copied()
            .collect();
        if inked.is_empty() {
            return true;
        }
        let (x0, x1) = x_bounds(&inked);
        // Some rule above the line and some rule at or below it are drawn
        // over its ink: the nearest rules need not be, since an underline
        // inside a cell joins the lattice as a rule of its own with the
        // reach of that cell alone.
        let over = |reach: &std::ops::Range<f32>| {
            x0 < reach.end + RULING_SNAP_TOLERANCE && x1 > reach.start - RULING_SNAP_TOLERANCE
        };
        let rules = self.ys.iter().zip(&self.reach);
        rules.clone().any(|(&y, reach)| y > group.y && over(reach))
            && rules.clone().any(|(&y, reach)| y <= group.y && over(reach))
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

/// The most lines of text that may stand between two stacked grids for
/// them to be one table: a statement rules each section's rows and leaves
/// the section label between two sections unruled, and a label wraps to
/// two lines at most.
const STACKED_GRID_LINES: usize = 2;
/// The gap between two stacked grids, in multiples of the type size of the
/// lines standing in it, beyond which they are two tables.
const STACKED_GRID_GAP: f32 = 3.0;

/// True when the gap between `above` and `below`, two grids on the same
/// drawn verticals, is short enough to hold a section label and nothing
/// more, with `lines` inked lines of type `size` standing in it: an empty
/// gap may be twice the snap wide; a populated one holds at most
/// [`STACKED_GRID_LINES`] lines and is no taller than [`STACKED_GRID_GAP`]
/// times their type size.
fn stacked_gap(above: &RuledGrid, below: &RuledGrid, lines: usize, size: f32, snap: f32) -> bool {
    if above.open || below.open || above.xs.len() != below.xs.len() {
        return false;
    }
    if above
        .xs
        .iter()
        .zip(&below.xs)
        .any(|(a, b)| (a - b).abs() > snap)
    {
        return false;
    }
    let gap = above.ys[0] - below.ys[below.ys.len() - 1];
    if gap < -snap {
        return false;
    }
    if lines == 0 {
        return gap <= 2.0 * snap;
    }
    lines <= STACKED_GRID_LINES && gap <= STACKED_GRID_GAP * size
}

/// The grids with each stack of sections on the same verticals folded into
/// one lattice, for the segmentation alone: a flow standing between two
/// sections of one statement belongs with them. The claims are read
/// section by section and merged afterwards by [`merge_stacked`]. `grids`
/// arrive topmost first and leave so.
fn stack_hulls(grids: &[RuledGrid], spans: &[TextSpan], snap: f32) -> Vec<RuledGrid> {
    let mut hulls: Vec<RuledGrid> = Vec::new();
    for grid in grids {
        let Some(above) = hulls.last_mut() else {
            hulls.push(grid.clone());
            continue;
        };
        let top = grid.ys[grid.ys.len() - 1];
        let bottom = above.ys[0];
        let between: Vec<&TextSpan> = spans
            .iter()
            .filter(|span| !blank(&span.text) && top < span.y && span.y < bottom)
            .collect();
        let size = median(between.iter().map(|span| span.size).collect());
        if !stacked_gap(above, grid, baseline_count(&between), size, snap) {
            hulls.push(grid.clone());
            continue;
        }
        let shared = bottom - top <= snap;
        let keep = grid.ys.len() - usize::from(shared);
        above.ys.splice(0..0, grid.ys[..keep].iter().copied());
        above.reach.splice(0..0, grid.reach[..keep].iter().cloned());
        above.boxed = above.boxed && grid.boxed;
    }
    hulls
}

/// Claims stacked down the page on the same drawn verticals merged into
/// one table: the sections of a financial statement, ruled section by
/// section with an unruled label between them. Each section's rows are
/// read under its own rules first, so a header wrapped inside one box and
/// a body band inferred from one section's lines come through the merge
/// unchanged; the lines in the gap join as the label rows [`stacked`]
/// reads them. `claims` arrive topmost first and leave so.
fn merge_stacked(
    claims: Vec<(GridClaim, &RuledGrid)>,
    groups: &[Group],
    snap: f32,
) -> Vec<GridClaim> {
    let mut merged: Vec<(GridClaim, &RuledGrid)> = Vec::new();
    for (claim, grid) in claims {
        let Some((above, above_grid)) = merged.last_mut() else {
            merged.push((claim, grid));
            continue;
        };
        let Some(labels) = stacked(above, above_grid, &claim, grid, groups, snap) else {
            merged.push((claim, grid));
            continue;
        };
        above.rows.extend(labels);
        above.rows.extend(claim.rows);
        above.range.end = claim.range.end;
        // The next gap is measured from the lowest section merged so far.
        *above_grid = grid;
        above.bbox = BBox {
            x0: above.bbox.x0.min(claim.bbox.x0),
            y0: above.bbox.y0.min(claim.bbox.y0),
            x1: above.bbox.x1.max(claim.bbox.x1),
            y1: above.bbox.y1.max(claim.bbox.y1),
        };
    }
    merged.into_iter().map(|(claim, _)| claim).collect()
}

/// The rows the gap between two claims adds when `below` continues
/// `above`: the same number of columns, a gap between their rules that
/// [`stacked_gap`] accepts, and every line in it that neither claim took
/// a row under the columns populating its first cell alone. `None` when
/// they are two tables, and when a line in the gap is no label: prose
/// between two tables, not a section label inside one.
fn stacked(
    above: &GridClaim,
    above_grid: &RuledGrid,
    below: &GridClaim,
    below_grid: &RuledGrid,
    groups: &[Group],
    snap: f32,
) -> Option<Vec<Vec<Cell>>> {
    if above.columns.len() != below.columns.len() {
        return None;
    }
    // A section opening with the column heads of the one above it is a
    // table of its own: a rate table repeats its heads over every section,
    // a statement's sections never do. A head wrapped onto two lines in one
    // box and set on one in the other spells the same words.
    let heads = |claim: &GridClaim| -> Vec<String> {
        claim.rows.first().map_or_else(Vec::new, |row| {
            row.iter()
                .map(|cell| {
                    cell_text(cell)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        })
    };
    if heads(above) == heads(below) {
        return None;
    }
    let inked = |group: &&Group| group.spans.iter().any(|span| !blank(&span.text));
    let top = below_grid.ys[below_grid.ys.len() - 1];
    let bottom = above_grid.ys[0];
    let between: Vec<&Group> = groups
        .iter()
        .filter(|group| top < group.y && group.y < bottom)
        .filter(inked)
        .collect();
    let size = median(between.iter().map(|group| group.size).collect());
    if !stacked_gap(above_grid, below_grid, between.len(), size, snap) {
        return None;
    }
    let unclaimed: Vec<&Group> = groups[above.range.end..below.range.start]
        .iter()
        .filter(inked)
        .collect();
    let mut labels = Vec::with_capacity(unclaimed.len());
    for group in unclaimed {
        let row = table_row(group, &above.columns)?;
        if !label_row(&row) {
            return None;
        }
        labels.push(row);
    }
    Some(labels)
}

/// The page's ruled grids: vertical and horizontal rulings clustered into
/// drawn lines, lines joined where they cross, and every connected lattice
/// of at least [`RULED_GRID_MIN_VERTICALS`] verticals and
/// [`RULED_GRID_MIN_HORIZONTALS`] horizontals kept, topmost first. A ruling
/// is vertical when it runs farther in y than in x; extraction already
/// snapped it exactly axis-aligned.
fn ruled_grids(rulings: &[Ruling], snap: f32) -> Vec<RuledGrid> {
    if rulings.is_empty() {
        return Vec::new();
    }
    let vertical = |r: &&Ruling| r.end.y - r.start.y > r.end.x - r.start.x;
    let verticals = grid_lines(
        rulings
            .iter()
            .filter(vertical)
            .map(|r| (r.start.x, r.start.y..r.end.y)),
        snap,
    );
    let horizontals = grid_lines(
        rulings
            .iter()
            .filter(|r| !vertical(r))
            .map(|r| (r.start.y, r.start.x..r.end.x)),
        snap,
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
            lattice(&component_verticals, &component_horizontals, snap)
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
/// Pages with more free horizontal rules than this skip open-ruled
/// detection outright: hundreds of stacked rules are graph paper or a
/// form's writing lines, not tables, and the gap-splitting recursion runs
/// one frame per rule in a cluster.
const OPEN_RULED_MAX_RULES: usize = 256;
/// A cluster of at least this many rules whose gaps all agree within
/// [`OPEN_RULED_EVEN_STACK_RATIO`] is a chart's plot grid or ruled paper —
/// identical bands drawn as guides. A table's row heights follow its rows'
/// content and never come out this uniform over so many rules, so the
/// whole cluster stands down; splitting it would only mine the corner its
/// labels hug. Rules inside a drawn lattice never reach here, so a fully
/// boxed zebra table keeps its evenly ruled rows.
const OPEN_RULED_EVEN_STACK: usize = 5;
const OPEN_RULED_EVEN_STACK_RATIO: f32 = 1.15;

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
    let horizontals: Vec<(f32, f32, f32)> = rulings
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
    if horizontals.len() < 2 || horizontals.len() > OPEN_RULED_MAX_RULES {
        return Vec::new();
    }
    // Joined, and so sorted by y.
    let horizontals = joined_rules(horizontals);
    // Every candidate region is a y-range query over the page's inked
    // spans; one ascending sort serves them all, built the first time a
    // cluster asks for it, and a whitespace-only span is out of every
    // region before any candidate looks.
    let mut inked: Option<Vec<&TextSpan>> = None;

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
        if even_stack(&ys) {
            continue;
        }
        let inked = inked.get_or_insert_with(|| {
            let mut inked: Vec<&TextSpan> = spans.iter().filter(|s| !blank(&s.text)).collect();
            inked.sort_by(|a, b| a.y.total_cmp(&b.y));
            inked
        });
        open_ruled_split(inked, &ys, seed_x0, seed_x1, taken, &mut grids);
    }
    grids
}

/// How far apart, in points, two collinear rule segments may end and begin
/// and still be one drawn rule: a producer draws a table's rule cell by
/// cell, each segment meeting the next end to end, where the underlines
/// of separate column heads leave the gutter between them unruled.
const RULE_JOIN_GAP: f32 = 1.0;

/// `rules` as `(y, x0, x1)` with the segments of one drawn rule joined:
/// collinear within [`RULE_JOIN_GAP`], each meeting the last within it.
fn joined_rules(mut rules: Vec<(f32, f32, f32)>) -> Vec<(f32, f32, f32)> {
    rules.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut joined: Vec<(f32, f32, f32)> = Vec::with_capacity(rules.len());
    for rule in rules {
        // The dots and dashes of a leader line meet end to end too, and
        // are no rule joined: only a segment at least a snap long joins.
        let segment = rule.2 - rule.1 >= RULING_SNAP_TOLERANCE;
        match joined.last_mut() {
            Some(last)
                if segment
                    && last.2 - last.1 >= RULING_SNAP_TOLERANCE
                    && (rule.0 - last.0).abs() <= RULE_JOIN_GAP
                    && rule.1 <= last.2 + RULE_JOIN_GAP =>
            {
                last.2 = last.2.max(rule.2);
            }
            _ => joined.push(rule),
        }
    }
    joined
}

/// True for [`OPEN_RULED_EVEN_STACK`] or more rules at near-identical gaps:
/// a plot grid or ruled paper, never a table.
fn even_stack(ys: &[f32]) -> bool {
    if ys.len() < OPEN_RULED_EVEN_STACK {
        return false;
    }
    let gaps = ys.windows(2).map(|pair| pair[1] - pair[0]);
    let smallest = gaps.clone().fold(f32::INFINITY, f32::min);
    let largest = gaps.fold(f32::NEG_INFINITY, f32::max);
    smallest > 0.0 && largest <= OPEN_RULED_EVEN_STACK_RATIO * smallest
}

/// One rule cluster as a table candidate, splitting at the largest rule
/// gap when the gates reject it whole. `inked` is the page's inked spans in
/// ascending y.
fn open_ruled_split(
    inked: &[&TextSpan],
    ys: &[f32],
    x0: f32,
    x1: f32,
    taken: &[RuledGrid],
    out: &mut Vec<RuledGrid>,
) {
    if ys.len() < 2 {
        return;
    }
    if let Some(grid) = open_ruled_candidate(inked, ys, x0, x1, taken) {
        out.push(grid);
        return;
    }
    let widest = (1..ys.len())
        .max_by(|&a, &b| (ys[a] - ys[a - 1]).total_cmp(&(ys[b] - ys[b - 1])))
        .expect("two rules have a gap");
    open_ruled_split(inked, &ys[..widest], x0, x1, taken, out);
    open_ruled_split(inked, &ys[widest..], x0, x1, taken, out);
}

/// The gates and column inference for one bracketed region; `None` sends
/// the cluster to the split. `inked` is the page's inked spans in ascending
/// y, so the region between two rules is one binary-searched slice.
fn open_ruled_candidate(
    inked: &[&TextSpan],
    ys: &[f32],
    x0: f32,
    x1: f32,
    taken: &[RuledGrid],
) -> Option<RuledGrid> {
    let (y_lo, y_hi) = (ys[0], ys[ys.len() - 1]);
    let from = inked.partition_point(|s| s.y < y_lo);
    let to = inked.partition_point(|s| s.y < y_hi);
    let region: Vec<&TextSpan> = inked[from..to.max(from)]
        .iter()
        .copied()
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
            Some(line) if (line[0].y - span.y).abs() <= 0.5 * line[0].size.max(span.size) => {
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
    // A banner or title row spanning the whole table crosses every column
    // boundary and is a colspan row, not a veto; text that is not a table
    // crosses on most of its lines. A quarter of the lines is the cut.
    boundaries.retain(|mid| {
        let crossing = lines
            .iter()
            .filter(|line| line.iter().any(|s| s.bbox.x0 < *mid && *mid < s.bbox.x1))
            .count();
        4 * crossing <= lines.len()
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
    let reach = vec![xs[0]..xs[xs.len() - 1]; ys.len()];
    let grid = RuledGrid {
        xs,
        ys: ys.to_vec(),
        reach,
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

/// How close two rulings may lie and still be one drawn line on this page:
/// [`RULING_SNAP_TOLERANCE`], or less where the type the close rules
/// bracket is small enough that rows are ruled closer than that. A rate
/// table set in 4-point type rules every row, 5.6 points apart; snapped at
/// 6 the rules chain into three lines and the table folds into two rows.
/// The type is measured between the close rules themselves, so a small
/// table under a page of large body text keeps its rows, and a caption in
/// small type beside a table in large type does not tighten that table's
/// rules.
fn ruling_snap(spans: &[TextSpan], rulings: &[Ruling]) -> f32 {
    // The size pass over the page's spans is paid only where the answer
    // can differ: two horizontal rules within the snap of each other.
    let mut ys: Vec<f32> = rulings
        .iter()
        .filter(|r| r.end.x - r.start.x >= r.end.y - r.start.y)
        .map(|r| r.start.y)
        .collect();
    ys.sort_by(f32::total_cmp);
    let close: Vec<&[f32]> = ys
        .windows(2)
        .filter(|pair| pair[1] - pair[0] <= RULING_SNAP_TOLERANCE)
        .collect();
    if close.is_empty() {
        return RULING_SNAP_TOLERANCE;
    }
    let lo = close.iter().map(|pair| pair[0]).fold(f32::MAX, f32::min);
    let hi = close.iter().map(|pair| pair[1]).fold(f32::MIN, f32::max);
    let sizes: Vec<f32> = spans
        .iter()
        .filter(|span| !blank(&span.text) && (lo..=hi).contains(&span.y))
        .map(|span| span.size)
        .collect();
    let body = if sizes.is_empty() {
        size_stats(&[spans]).body
    } else {
        median(sizes)
    };
    if body <= 0.0 {
        return RULING_SNAP_TOLERANCE;
    }
    RULING_SNAP_TOLERANCE.min(RULING_SNAP_OF_SIZE * body)
}

/// Rulings on one axis as drawn lines: grouped by their constant coordinate
/// within `snap`, each group's near-touching extents merged. Extents
/// farther apart stay separate lines at the same position — the x two
/// stacked tables' borders share must not weld them into one lattice.
fn grid_lines(
    rulings: impl Iterator<Item = (f32, std::ops::Range<f32>)>,
    snap: f32,
) -> Vec<GridLine> {
    let mut all: Vec<(f32, std::ops::Range<f32>)> = rulings.collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut lines: Vec<GridLine> = Vec::new();
    let mut cluster: Vec<(f32, std::ops::Range<f32>)> = Vec::new();
    for line in all {
        if let Some(last) = cluster.last() {
            if line.0 - last.0 > snap {
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
fn lattice(verticals: &[&GridLine], horizontals: &[&GridLine], snap: f32) -> Option<RuledGrid> {
    let mut xs = distinct_positions(verticals, snap);
    let mut ys = distinct_positions(horizontals, snap);
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
    let width = xs[0]..xs[xs.len() - 1];
    let reach = ys
        .iter()
        .map(|&y| {
            let drawn = horizontals
                .iter()
                .filter(|line| (line.position - y).abs() <= snap)
                .fold(None::<std::ops::Range<f32>>, |reach, line| match reach {
                    Some(reach) => {
                        Some(reach.start.min(line.extent.start)..reach.end.max(line.extent.end))
                    }
                    None => Some(line.extent.clone()),
                });
            drawn.unwrap_or_else(|| width.clone())
        })
        .collect();
    Some(RuledGrid {
        xs,
        ys,
        reach,
        boxed,
        open: false,
    })
}

/// The lines' positions, ascending, neighbours within `snap` collapsed to
/// the first.
fn distinct_positions(lines: &[&GridLine], snap: f32) -> Vec<f32> {
    let mut positions: Vec<f32> = lines.iter().map(|line| line.position).collect();
    positions.sort_by(f32::total_cmp);
    positions.dedup_by(|next, kept| *next - *kept <= snap);
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
    columns: Vec<std::ops::Range<f32>>,
    rows: Vec<Vec<Cell>>,
    bbox: BBox,
}

/// Every grid's claim on the segment's lines, disjoint and top-down, with
/// stacked claims merged by [`merge_stacked`]. Grids are tried topmost
/// first, so of two lattices claiming the same lines the higher wins; a
/// grid that fails its gates claims nothing and its lines stay available
/// to the lane attempt. Lane-occupancy gates do not apply to a claim — a
/// drawn single-column box is a table no lane could show — but every line
/// still passes [`table_row`]'s word-gap cell gate.
fn grid_claims(
    groups: &[Group],
    grids: &[RuledGrid],
    hulls: &[RuledGrid],
    snap: f32,
) -> Vec<GridClaim> {
    let mut claims: Vec<(GridClaim, &RuledGrid)> = Vec::new();
    for grid in grids {
        let Some(claim) = grid_claim(groups, grid, grids, hulls) else {
            continue;
        };
        let taken = claims.iter().any(|(held, _)| {
            held.range.start < claim.range.end && claim.range.start < held.range.end
        });
        if taken {
            continue;
        }
        claims.push((claim, grid));
    }
    claims.sort_by_key(|(claim, _)| claim.range.start);
    merge_stacked(claims, groups, snap)
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
fn grid_claim(
    groups: &[Group],
    grid: &RuledGrid,
    grids: &[RuledGrid],
    hulls: &[RuledGrid],
) -> Option<GridClaim> {
    let lo = groups.iter().position(|group| grid.holds(group.y))?;
    let inside = groups[lo..]
        .iter()
        .take_while(|group| grid.holds(group.y))
        .count();
    let hi = lo + inside;
    // A drawn lattice's rows lie under its rules; a line inside its bands
    // with no rule drawn over it belongs to something the lattice welded
    // onto, a chart's frame or a form's margin, and the lattice is no table.
    if !grid.open && groups[lo..hi].iter().any(|group| !grid.under_rules(group)) {
        return None;
    }
    let stack = stack_lines(groups, grid, hulls).unwrap_or(lo..hi);
    let mut columns = lane_split_columns(open_columns(&groups[lo..hi], grid), &groups[stack]);
    let top = header_reach(groups, lo, hi, grid, grids, &columns);
    // A claim grows only from lines its drawn columns already read as
    // rows: the grown claim's columns come from the lanes of the whole,
    // and lanes must not admit what the rules refused. The double rules
    // under two tables' totals pair up as one open lattice spanning both,
    // and the lines it holds run across its two drawn columns.
    let rows_under_rules = groups[lo..hi]
        .iter()
        .all(|group| table_row(group, &columns).is_some());
    let end = if grid.open && rows_under_rules {
        open_reach(groups, lo, hi, grid, grids)
    } else {
        hi
    };
    if end > hi {
        // A grown claim's verticals were inferred from one section's text;
        // the columns of the whole are the lanes its lines leave. The lines
        // leaving a lane of their own vote first; a one-run line then joins
        // unless its run covers one of their lanes whole. A title set over
        // two of the columns runs across the gap between them and reads as
        // one cell over both, not as a reason to merge the columns under
        // it; a header word starting inside a lane narrows the lane and
        // keeps a column to start in. The header lines above the top rule
        // stay out likewise.
        let spans: Vec<&TextSpan> = groups[lo..end]
            .iter()
            .flat_map(|group| group.spans.iter().copied())
            .filter(|span| !blank(&span.text))
            .collect();
        let claimed = &groups[lo..end];
        let min_gap = gutter_min(claimed);
        let mut occupied: Vec<std::ops::Range<f32>> = Vec::new();
        let mut single_runs: Vec<std::ops::Range<f32>> = Vec::new();
        for group in claimed {
            let mut own: Vec<std::ops::Range<f32>> = Vec::new();
            for span in group.spans.iter().filter(|span| !blank(&span.text)) {
                add_ink(&mut own, span.x.min(span.end_x)..span.x.max(span.end_x));
            }
            if ink_gaps(&own, min_gap).is_empty() {
                single_runs.extend(own);
                continue;
            }
            for ink in own {
                add_ink(&mut occupied, ink);
            }
        }
        let voted = ink_gaps(&occupied, min_gap);
        for run in single_runs {
            if voted
                .iter()
                .any(|lane| run.start <= lane.start && lane.end <= run.end)
            {
                continue;
            }
            add_ink(&mut occupied, run);
        }
        columns = cell_columns(&spans, &ink_gaps(&occupied, min_gap));
    }
    let mut rows = Vec::with_capacity(end - top);
    for group in &groups[top..lo] {
        rows.push(table_row(group, &columns)?);
    }
    for band in groups[lo..hi].chunk_by(|a, b| grid.band_of(a.y) == grid.band_of(b.y)) {
        let mut lines = Vec::with_capacity(band.len());
        for group in band {
            lines.push(table_row(group, &columns)?);
        }
        // An open lattice's rules bracket its sections, so every band of
        // two lines or more is a section whose lines are rows; a drawn
        // lattice infers rows only in the band holding most of its lines,
        // the others being wrapped single rows.
        let infer_floor = if grid.open { 2 } else { BAND_INFER_MIN_LINES };
        if lines.len() >= infer_floor && (grid.open || 2 * lines.len() > hi - lo) {
            rows.append(&mut anchored_rows(lines, columns.len(), grid.open));
            continue;
        }
        if lines.len() >= 2 && figure_records(&lines, columns.len()) {
            rows.append(&mut lines);
            continue;
        }
        rows.push(logical_row(lines, columns.len()));
    }
    for group in &groups[hi..end] {
        rows.push(table_row(group, &columns)?);
    }
    // A band holding nothing but whitespace spans is the page's padding,
    // not a row: a row of blank cells says nothing.
    rows.retain(|row| row.iter().any(inked_cell));
    // A claim grown over unruled lines stops short of a line repeating its
    // head: the second table under the same years is its own, and its rules
    // claim it in turn.
    let mut end = end;
    if grid.open && end > hi {
        if let Some(split) = repeated_head(&rows) {
            let y = row_y(&rows[split]);
            let cut = groups[top..end]
                .iter()
                .position(|group| group.y <= y + 0.5 * group.size)
                .map(|cut| top + cut);
            if let Some(cut) = cut.filter(|cut| *cut >= hi) {
                rows.truncate(split);
                end = cut;
            }
        }
    }
    // An open lattice chains the rules of two tables set to the same
    // columns, and the prose between the tables lies inside the drawn
    // width and reads as one cell over every column. A row like that
    // between two records is prose, and the lattice is two tables the lane
    // attempt reads apart. At the top or the bottom it is the table's own
    // title or note; a section label populates the first column and a
    // spanning note some of the columns.
    if grid.open && columns.len() >= TABLE_MIN_LANES {
        let spanning: Vec<bool> = rows
            .iter()
            .map(|row| spans_every_column(row, columns.len()))
            .collect();
        let first = spanning.iter().position(|spans| !spans);
        let last = spanning.iter().rposition(|spans| !spans);
        if let (Some(first), Some(last)) = (first, last) {
            if spanning[first..=last].iter().any(|spans| *spans) {
                return None;
            }
        }
    }
    if rows.len() < TABLE_MIN_ROWS
        && !((grid.boxed || grid.open) && rows.len() >= RULED_BOXED_MIN_ROWS)
    {
        return None;
    }
    Some(GridClaim {
        range: top..end,
        columns,
        rows,
        bbox: grid.bbox(),
    })
}

/// True when the row is one inked cell over all `columns` columns.
fn spans_every_column(row: &[Cell], columns: usize) -> bool {
    row.len() == 1 && row[0].colspan as usize == columns && inked_cell(&row[0])
}

/// The lines the stack holding `grid` covers, `hulls` being the stacks
/// [`stack_hulls`] folded. The sections of one statement split their drawn
/// columns at the lanes all their lines leave, so every section reads
/// under the same columns and their claims merge; a section read alone
/// would split at the lanes its own few lines leave. `None` when no hull
/// holds the grid.
fn stack_lines(
    groups: &[Group],
    grid: &RuledGrid,
    hulls: &[RuledGrid],
) -> Option<std::ops::Range<usize>> {
    let mid = (grid.ys[0] + grid.ys[grid.ys.len() - 1]) / 2.0;
    let hull = hulls.iter().find(|hull| hull.holds(mid))?;
    let start = groups.iter().position(|group| hull.holds(group.y))?;
    let count = groups[start..]
        .iter()
        .take_while(|group| hull.holds(group.y))
        .count();
    Some(start..start + count)
}

/// The narrowest lane that splits a drawn column. A lane this wide, kept
/// clear by every line of the claim, is a column boundary the producer
/// left unruled: a statement rules its year columns and sets the label,
/// the currency sign and the amount in the first box.
const RULED_LANE_MIN_WIDTH: f32 = 2.0 * GUTTER_MIN_WIDTH;

/// `columns` with each drawn column split at the lanes the claim's lines
/// leave clear inside it, a snap's width in from either rule so a lane
/// hugging a rule is that rule's own margin.
fn lane_split_columns(
    columns: Vec<std::ops::Range<f32>>,
    groups: &[Group],
) -> Vec<std::ops::Range<f32>> {
    let lanes: Vec<std::ops::Range<f32>> = lanes_of(groups, gutter_min(groups))
        .into_iter()
        .filter(|lane| lane.end - lane.start >= RULED_LANE_MIN_WIDTH)
        .collect();
    if lanes.is_empty() {
        return columns;
    }
    let mut out = Vec::with_capacity(columns.len() + lanes.len());
    for column in columns {
        let mut start = column.start;
        for lane in lanes.iter().filter(|lane| {
            lane.start > column.start + RULING_SNAP_TOLERANCE
                && lane.end < column.end - RULING_SNAP_TOLERANCE
        }) {
            out.push(start..lane.start);
            start = lane.end;
        }
        out.push(start..column.end);
    }
    out
}

/// How far above a grid's top rule its header may stand, in multiples of
/// the row pitch inside the grid.
const HEADER_REACH: f32 = 2.5;
/// The most lines a header above the top rule may run to.
const HEADER_REACH_LINES: usize = 6;

/// The index of the topmost header line above the grid, or `lo` when the
/// header is inside the box. Lines are walked upward from the top rule,
/// each within [`HEADER_REACH`] row pitches of the line below it, and each
/// reading as a header line: its cells one column wide each, either two
/// or more of them or a single one in the first column. The walk keeps the
/// lines down to the topmost one populating [`TABLE_MIN_ROW_CELLS`] cells.
/// A statement rules its body and sets the column heads above the box; a
/// caption or a paragraph above the box runs its words across the column
/// boundaries and stays out, and so does a line another grid holds.
fn header_reach(
    groups: &[Group],
    lo: usize,
    hi: usize,
    grid: &RuledGrid,
    grids: &[RuledGrid],
    columns: &[std::ops::Range<f32>],
) -> usize {
    if hi - lo < 2 {
        return lo;
    }
    let pitch = median(
        groups[lo..hi]
            .windows(2)
            .map(|pair| pair[0].y - pair[1].y)
            .collect(),
    );
    if pitch <= 0.0 {
        return lo;
    }
    let limit = HEADER_REACH * pitch;
    let mut below = grid.ys[grid.ys.len() - 1];
    let mut top = lo;
    for index in (lo.saturating_sub(HEADER_REACH_LINES)..lo).rev() {
        let group = &groups[index];
        if group.y - below > limit || held_elsewhere(grids, grid, group.y) {
            break;
        }
        let Some(row) = table_row(group, columns) else {
            break;
        };
        if !header_line(&row) {
            break;
        }
        below = group.y;
        if populated_cells(&row) >= TABLE_MIN_ROW_CELLS {
            top = index;
        }
    }
    top
}

/// The end of an open-ruled claim grown downward over the lines below its
/// closing rule that keep every lane its own lines leave, each within
/// [`TABLE_LABEL_GAP`] row pitches of the line above it: an open-ruled
/// statement underlines its header and each section's total, so its rules
/// bracket one section at a time while the sections share the columns. The
/// lanes, not the inferred verticals, are the test: a vertical inferred
/// from one section's short labels lands inside the next section's longer
/// ones, while a lane narrows and survives. A line running across a lane
/// is prose and ends the growth, as does a line another grid holds; the
/// lines at the end with a single cell are trimmed back off, so a note
/// under the table does not join it.
fn open_reach(
    groups: &[Group],
    lo: usize,
    hi: usize,
    grid: &RuledGrid,
    grids: &[RuledGrid],
) -> usize {
    if hi - lo < 2 {
        return hi;
    }
    let pitch = median(
        groups[lo..hi]
            .windows(2)
            .map(|pair| pair[0].y - pair[1].y)
            .collect(),
    );
    if pitch <= 0.0 {
        return hi;
    }
    let limit = TABLE_LABEL_GAP * pitch;
    let min_gap = gutter_min(&groups[lo..hi]);
    let mut occupied: Vec<std::ops::Range<f32>> = Vec::new();
    for group in &groups[lo..hi] {
        for span in group.spans.iter().filter(|span| !blank(&span.text)) {
            add_ink(
                &mut occupied,
                span.x.min(span.end_x)..span.x.max(span.end_x),
            );
        }
    }
    let lanes = ink_gaps(&occupied, min_gap).len();
    if lanes == 0 {
        return hi;
    }
    let mut above = groups[hi - 1].y;
    let mut last_record = hi;
    for (index, group) in groups.iter().enumerate().skip(hi) {
        if above - group.y > limit || held_elsewhere(grids, grid, group.y) {
            break;
        }
        let mut next = occupied.clone();
        let mut own: Vec<std::ops::Range<f32>> = Vec::new();
        for span in group.spans.iter().filter(|span| !blank(&span.text)) {
            let ink = span.x.min(span.end_x)..span.x.max(span.end_x);
            add_ink(&mut next, ink.clone());
            add_ink(&mut own, ink);
        }
        if ink_gaps(&next, min_gap).len() < lanes {
            break;
        }
        occupied = next;
        above = group.y;
        if !ink_gaps(&own, min_gap).is_empty() {
            last_record = index + 1;
        }
    }
    last_record
}

/// True when a drawn lattice other than `grid` holds the baseline `y`. A
/// header above one box and the unruled lines under an open lattice never
/// lie inside another box: a claim reaching into one would overlap its
/// claim, and the lower of the two would be dropped. Open lattices do not
/// count: an open-ruled statement brackets each section with its own
/// rules, and growing over the next section's bracket is what the growth
/// is for.
fn held_elsewhere(grids: &[RuledGrid], grid: &RuledGrid, y: f32) -> bool {
    grids
        .iter()
        .any(|other| !other.open && !std::ptr::eq(other, grid) && other.holds(y))
}

/// True when a line outside the rules reads as a row of the grid rather
/// than prose running across it: every populated cell is one column wide,
/// and the row populates two cells or more, or the first cell alone.
fn header_line(row: &[Cell]) -> bool {
    let populated: Vec<&Cell> = row.iter().filter(|cell| cell.line.is_some()).collect();
    if populated.iter().any(|cell| cell.colspan > 1) {
        return false;
    }
    populated.len() >= TABLE_MIN_ROW_CELLS || label_row(row)
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
        .filter(|span| !blank(&span.text))
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
    // In a band of top-aligned records, every line that draws in the
    // anchor column opens one. An anchor line that is not a record — a
    // group label filling only the anchor — says the band is not records
    // at all, and it merges whole.
    if lines
        .iter()
        .any(|line| populates(line, anchor) && !opens(line))
    {
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

/// True when a band's lines are each a record in their own right: every
/// line populates at least three quarters of the columns, and at least
/// half of all the populated cells open as figures. A timetable or a rate
/// table rules every few rows, and the lines between two rules are rows,
/// not one wrapped row. A header wrapped over two lines is words and still
/// merges; so does a band of centered cells whose lines each fill half the
/// columns, which is one record whose cells sit on different lines.
fn figure_records(lines: &[Vec<Cell>], columns: usize) -> bool {
    let mut populated = 0usize;
    let mut figures = 0usize;
    for line in lines {
        let cells: Vec<&Cell> = line.iter().filter(|cell| cell.line.is_some()).collect();
        if 4 * cells.len() < 3 * columns {
            return false;
        }
        populated += cells.len();
        figures += cells.iter().filter(|cell| figure_cell(cell)).count();
    }
    2 * figures >= populated
}

/// True when a cell opens as a figure: a digit, a currency sign, a
/// parenthesis or a dash standing for nil.
fn figure_cell(cell: &Cell) -> bool {
    cell_text(cell).trim_start().starts_with(|c: char| {
        c.is_ascii_digit()
            || AMOUNT_SIGNS.contains(&c)
            || matches!(c, '€' | '(' | '-' | '\u{2013}' | '\u{2014}')
    })
}

/// True when the cell carries text that paints: not a lineless cell, and
/// not one holding whitespace spans alone.
fn inked_cell(cell: &Cell) -> bool {
    cell.line
        .as_ref()
        .is_some_and(|line| line.inlines.iter().any(|inline| !blank(&inline.text)))
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
    // The one-lane stretches long enough for a two-column table, kept for
    // the second pass: one lane is weaker evidence, and a three-column
    // table anywhere in the segment comes first.
    let mut pairs: Vec<(usize, LaneRun, f32)> = Vec::new();
    for start in 0..groups.len() {
        // The lane width follows the type of the run's own first lines: a
        // statement's 7-point rows stand in a column of 10-point prose.
        let min_gap = gutter_min(&groups[start..(start + TABLE_MIN_ROWS).min(groups.len())]);
        let (two, one) = lane_runs(groups, start, min_gap);
        if one.end - start >= PAIR_MIN_ROWS {
            pairs.push((start, one, min_gap));
        }
        if two.end - start < TABLE_MIN_ROWS {
            continue;
        }
        if let Some(band) = grid(groups, start, two.end, &two.lanes, min_gap, TABLE_MIN_LANES) {
            return Some(band);
        }
    }
    // No stretch keeps two lanes: a two-column table is looked for next,
    // and [`pair_table`] asks more of it. A one-lane stretch that is no
    // table settles the starts inside it that leave the same lane: they
    // read the same rows, and a page of indented paragraphs would otherwise
    // pay a grid attempt per line. A start leaving another lane, a table
    // standing inside a numbered item's stretch, is tried on its own.
    let mut settled: Option<(usize, Vec<std::ops::Range<f32>>)> = None;
    for (start, run, min_gap) in pairs {
        if settled
            .as_ref()
            .is_some_and(|(until, lanes)| start < *until && same_lanes(lanes, &run.lanes))
        {
            continue;
        }
        settled = Some((run.end, run.lanes.clone()));
        let Some(band) = grid(groups, start, run.end, &run.lanes, min_gap, 1) else {
            continue;
        };
        if pair_table(&band.rows) {
            return Some(band);
        }
    }
    None
}

/// True when two lane sets are the same lanes: as many, each pair of them
/// overlapping.
fn same_lanes(a: &[std::ops::Range<f32>], b: &[std::ops::Range<f32>]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.start < y.end && y.start < x.end)
}

/// The rows a two-column band must populate on both sides: one lane is
/// what a list's markers or a form's labels leave too, and three rows of
/// it are not a table.
const PAIR_MIN_ROWS: usize = 4;
/// The widest the narrower column of a two-column band may run, as a share
/// of the band's width. A glossary's terms, a subsidiary list's
/// jurisdictions and a rate table's factors take a third of the width at
/// most; two columns of prose take near half each.
const PAIR_NARROW_SHARE: f32 = 1.0 / 3.0;

/// The gates a two-column band passes beyond a grid's: at least
/// [`PAIR_MIN_ROWS`] rows populating both cells; a first column that is
/// not mostly list markers, which would make the band a numbered or
/// bulleted list, and not mostly labels ending in a colon, which would
/// make it a form or a block of metadata; and a narrower column within
/// [`PAIR_NARROW_SHARE`] of the band's width, which two columns of prose
/// never are.
fn pair_table(rows: &[Vec<Cell>]) -> bool {
    if rows.iter().any(|row| row.len() > 2) {
        return false;
    }
    let records: Vec<&Vec<Cell>> = rows
        .iter()
        .filter(|row| row.iter().filter(|cell| inked_cell(cell)).count() >= 2)
        .collect();
    if records.len() < PAIR_MIN_ROWS {
        return false;
    }
    let firsts: Vec<String> = records
        .iter()
        .filter_map(|row| row.iter().find(|cell| inked_cell(cell)))
        .map(cell_text)
        .collect();
    if 2 * firsts.iter().filter(|text| marker_cell(text)).count() > firsts.len() {
        return false;
    }
    if 2 * firsts
        .iter()
        .filter(|text| text.trim_end().ends_with(':'))
        .count()
        > firsts.len()
    {
        return false;
    }
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut widths: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
    for row in &records {
        for (side, cell) in row
            .iter()
            .filter(|cell| inked_cell(cell))
            .take(2)
            .enumerate()
        {
            let Some(line) = cell.line.as_ref() else {
                continue;
            };
            lo = lo.min(line.x);
            hi = hi.max(line.end_x);
            widths[side].push(line.end_x - line.x);
        }
    }
    let width = hi - lo;
    if width <= 0.0 {
        return false;
    }
    let narrower = median(widths[0].clone()).min(median(widths[1].clone()));
    narrower <= PAIR_NARROW_SHARE * width
}

/// True when a cell holds nothing but a list marker: a bullet, one to three
/// digits or a single letter closed by `.` or `)`, or one to three digits
/// in brackets or parentheses.
fn marker_cell(text: &str) -> bool {
    let text = text.trim();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if chars.as_str().is_empty() && BULLETS.contains(&first) {
        return true;
    }
    let bracketed = text
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .or_else(|| {
            text.strip_prefix('(')
                .and_then(|rest| rest.strip_suffix(')'))
        });
    if let Some(inner) = bracketed {
        return (1..=3).contains(&inner.len()) && inner.chars().all(|c| c.is_ascii_digit());
    }
    let Some(body) = text.strip_suffix(['.', ')']) else {
        return false;
    };
    let digits = body.chars().all(|c| c.is_ascii_digit()) && (1..=3).contains(&body.len());
    let letter = body.chars().count() == 1 && body.chars().all(|c| c.is_ascii_alphabetic());
    digits || letter
}

/// A stretch of lines from one start that keeps some number of lanes: its
/// exclusive end and the lanes the whole stretch leaves.
struct LaneRun {
    end: usize,
    lanes: Vec<std::ops::Range<f32>>,
}

/// The stretches starting at `start` that keep [`TABLE_MIN_LANES`] lanes
/// and one lane, from a single scan of the lines: the two-lane stretch
/// ends where the lanes fall under two, the one-lane stretch where the
/// last lane closes. Ink is tracked as exact intervals, not histogram
/// bins: a column gap of a few points is real table structure that bin
/// rounding swallows. A whitespace-only span paints nothing: a producer's
/// padding standing in a gutter neither closes the lane nor opens a column
/// of its own.
fn lane_runs(groups: &[Group], start: usize, min_gap: f32) -> (LaneRun, LaneRun) {
    let mut occupied: Vec<std::ops::Range<f32>> = Vec::new();
    let mut lanes: Vec<std::ops::Range<f32>> = Vec::new();
    let mut two: Option<LaneRun> = None;
    let mut end = groups.len();
    for (offset, group) in groups[start..].iter().enumerate() {
        for span in group.spans.iter().filter(|span| !blank(&span.text)) {
            add_ink(
                &mut occupied,
                span.x.min(span.end_x)..span.x.max(span.end_x),
            );
        }
        let gaps = ink_gaps(&occupied, min_gap);
        if gaps.len() < TABLE_MIN_LANES && two.is_none() {
            two = Some(LaneRun {
                end: start + offset,
                lanes: lanes.clone(),
            });
        }
        if gaps.is_empty() {
            end = start + offset;
            break;
        }
        lanes = gaps;
    }
    let two = two.unwrap_or_else(|| LaneRun {
        end,
        lanes: lanes.clone(),
    });
    (two, LaneRun { end, lanes })
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

/// The gaps between consecutive ink intervals at least `min_gap` wide, the
/// interior ones by construction: whatever lies beyond the outermost ink
/// is margin, not lane.
fn ink_gaps(occupied: &[std::ops::Range<f32>], min_gap: f32) -> Vec<std::ops::Range<f32>> {
    occupied
        .windows(2)
        .filter(|pair| pair[1].start - pair[0].end >= min_gap)
        .map(|pair| pair[0].end..pair[1].start)
        .collect()
}

/// The narrowest lane as a share of the type size, where the type is small
/// enough that [`GUTTER_MIN_WIDTH`] would swallow a column gap: a statement
/// set in 7-point type separates its columns by six points, and a word
/// space there is under two.
const GUTTER_OF_SIZE: f32 = 0.7;

/// The narrowest lane between these lines' cells: [`GUTTER_MIN_WIDTH`], or
/// less in proportion where their type is small.
fn gutter_min(groups: &[Group]) -> f32 {
    let size = median(groups.iter().map(|group| group.size).collect());
    if size <= 0.0 {
        return GUTTER_MIN_WIDTH;
    }
    GUTTER_MIN_WIDTH.min(GUTTER_OF_SIZE * size)
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
    min_gap: f32,
    min_lanes: usize,
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
    let filled: Vec<usize> = (0..populated.len())
        .filter(|index| populated[*index])
        .collect();
    let (first, last) = even_stretch(inside, &rows, &filled)?;
    let stretch = lo + first..lo + last + 1;
    let rows = match own_rows(groups, start..end, stretch.clone(), min_gap, min_lanes) {
        Some(rows) => rows,
        None => {
            if populated_columns(&rows[first..=last], columns.len()) < min_lanes + 1 {
                return None;
            }
            rows.truncate(last + 1);
            rows.drain(..first);
            rows
        }
    };
    if contents_list(&rows) {
        return None;
    }
    Some(TableBand {
        rows,
        span: stretch,
        run_start: start,
    })
}

/// True when the rows read as a table of contents or an index: at least
/// [`TABLE_MIN_ROWS`] rows end in a page number, the numbers never fall
/// down the rows (two entries may share a page), and words come before
/// each number; a row populating one cell of words alone is a part
/// heading between entries. Front matter counts its pages in roman
/// numerals, which precede the arabic ones, and a page number may share
/// its cell with the last word of a wrapped title. Entry numbers, titles
/// and page numbers line up in lanes like any grid, but the list is prose
/// to the heading pass, not a table, and ground truth reads it so.
fn contents_list(rows: &[Vec<Cell>]) -> bool {
    let mut last = PageNumber::Roman(0);
    let mut entries = 0usize;
    for row in rows {
        let filled: Vec<&Cell> = row.iter().filter(|cell| cell.line.is_some()).collect();
        let Some((tail, entry)) = filled.split_last() else {
            return false;
        };
        let text = cell_text(tail);
        let text = text.trim();
        let (head, token) = text.rsplit_once(char::is_whitespace).unwrap_or(("", text));
        let Some(page) = page_number(token) else {
            if filled.len() == 1 && text.chars().any(|c| c.is_alphabetic()) {
                continue;
            }
            return false;
        };
        let worded = entry
            .iter()
            .map(|cell| cell_text(cell))
            .chain(std::iter::once(head.to_string()))
            .any(|words| words.chars().any(|c| c.is_alphabetic()));
        if !worded || !last.precedes(page) {
            return false;
        }
        last = page;
        entries += 1;
    }
    entries >= TABLE_MIN_ROWS
}

/// A page number in a table of contents: the roman numerals of the front
/// matter, or an arabic number.
#[derive(Clone, Copy)]
enum PageNumber {
    Roman(u32),
    Arabic(u32),
}

impl PageNumber {
    /// True when `next` may follow `self` down a contents list: the
    /// numbers never fall, and the arabic pages follow the roman ones.
    fn precedes(self, next: PageNumber) -> bool {
        match (self, next) {
            (PageNumber::Roman(a), PageNumber::Roman(b))
            | (PageNumber::Arabic(a), PageNumber::Arabic(b)) => a <= b,
            (PageNumber::Roman(_), PageNumber::Arabic(_)) => true,
            (PageNumber::Arabic(_), PageNumber::Roman(_)) => false,
        }
    }
}

/// The page number a token stands for, if any.
fn page_number(token: &str) -> Option<PageNumber> {
    if let Ok(number) = token.parse::<u32>() {
        return Some(PageNumber::Arabic(number));
    }
    roman_numeral(token).map(PageNumber::Roman)
}

/// The highest page roman-numbered front matter reaches. A preface or an
/// index runs to a few dozen pages; the words that happen to spell a
/// roman numeral, "ml", "mix", "dim", spell far larger ones.
const ROMAN_PAGE_MAX: u32 = 100;

/// The value of a token that spells a roman numeral in its standard form,
/// in either case, from one to [`ROMAN_PAGE_MAX`]; `None` for anything
/// else, "llc" and "ml" included.
fn roman_numeral(token: &str) -> Option<u32> {
    if token.is_empty() || token.len() > 8 {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    let values: Vec<i64> = lower
        .chars()
        .map(|c| match c {
            'i' => Some(1),
            'v' => Some(5),
            'x' => Some(10),
            'l' => Some(50),
            'c' => Some(100),
            'd' => Some(500),
            'm' => Some(1000),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let total: i64 = values
        .iter()
        .enumerate()
        .map(|(index, &value)| {
            if values.get(index + 1).is_some_and(|&next| next > value) {
                -value
            } else {
                value
            }
        })
        .sum();
    let value = u32::try_from(total).ok()?;
    ((1..=ROMAN_PAGE_MAX).contains(&value) && roman_of(value) == lower).then_some(value)
}

/// `value` written as a roman numeral in lowercase letters.
fn roman_of(mut value: u32) -> String {
    const STEPS: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (step, letters) in STEPS {
        while value >= step {
            out.push_str(letters);
            value -= step;
        }
    }
    out
}

/// The stretch's rows over the columns its own run lines leave. The run's
/// lanes were cut by everything else the run held — a page number far
/// below, a note — and a lane only those lines opened would be an empty
/// column in the grid. The lanes come from the run lines inside the stretch
/// alone, never from a row `merged_edges` added, since a header or total
/// row spanning a lane is a row of the grid precisely because it does not
/// cut the lane. `None` sends the caller back to the run's own columns.
fn own_rows(
    groups: &[Group],
    run: std::ops::Range<usize>,
    stretch: std::ops::Range<usize>,
    min_gap: f32,
    min_lanes: usize,
) -> Option<Vec<Vec<Cell>>> {
    let core = run.start.max(stretch.start)..run.end.min(stretch.end);
    if core.start >= core.end {
        return None;
    }
    let lanes = lanes_of(&groups[core], min_gap);
    if lanes.len() < min_lanes {
        return None;
    }
    let spans: Vec<&TextSpan> = groups[stretch.clone()]
        .iter()
        .flat_map(|group| group.spans.iter().copied())
        .collect();
    let columns = cell_columns(&spans, &lanes);
    let rows: Vec<Vec<Cell>> = groups[stretch]
        .iter()
        .map(|group| table_row(group, &columns))
        .collect::<Option<_>>()?;
    (populated_columns(&rows, columns.len()) > min_lanes).then_some(rows)
}

/// The longest run of populated rows whose neighbouring baselines never
/// step more than [`TABLE_ROW_GAP`] times the median step, as the first and
/// last index into `inside` it covers. The step is what separates one grid
/// from two blocks that happen to share columns, or from a page number
/// standing far below; only populated rows are measured, so a wrapped cell
/// standing alone in a hole cannot halve a step. The topmost run wins a
/// tie. `None` when no run holds [`TABLE_MIN_ROWS`] rows, and when the
/// baselines do not move at all.
fn even_stretch(inside: &[Group], rows: &[Vec<Cell>], filled: &[usize]) -> Option<(usize, usize)> {
    if filled.len() < TABLE_MIN_ROWS {
        return None;
    }
    let steps: Vec<f32> = filled
        .windows(2)
        .map(|pair| inside[pair[0]].y - inside[pair[1]].y)
        .collect();
    let pitch = median(steps.clone());
    let limit = TABLE_ROW_GAP * pitch;
    if limit <= 0.0 {
        return None;
    }
    let longer = |best: Option<(usize, usize)>, candidate: (usize, usize)| match best {
        Some((a, b)) if b - a >= candidate.1 - candidate.0 => Some((a, b)),
        _ => Some(candidate),
    };
    let mut best = None;
    let mut run_start = 0;
    for (index, step) in steps.iter().enumerate() {
        if *step <= limit || labels_between(inside, rows, filled[index], filled[index + 1], pitch) {
            continue;
        }
        best = longer(best, (run_start, index));
        run_start = index + 1;
    }
    best = longer(best, (run_start, filled.len() - 1));
    let (a, b) = best?;
    (b - a + 1 >= TABLE_MIN_ROWS).then(|| (filled[a], filled[b]))
}

/// How far, in row pitches, a section label may stand below the row above
/// it and stay inside the table: a statement leaves a blank line above
/// each section's label.
const TABLE_LABEL_GAP: f32 = 2.5;
/// How far, in row pitches, the first row of a section may stand below
/// its label: the label heads its rows directly, where a caption between
/// two tables leaves a blank line on both sides.
const TABLE_LABEL_BELOW: f32 = 1.5;

/// True when every line between the populated rows `a` and `b` is a
/// section label, a line populating the first cell alone, no step from one
/// line to the next across them exceeds [`TABLE_LABEL_GAP`] row pitches,
/// and the step from the last label down to `b` stays within
/// [`TABLE_LABEL_BELOW`]. The step from `a` to `b` itself exceeds the row
/// gap, or the question would not arise.
fn labels_between(inside: &[Group], rows: &[Vec<Cell>], a: usize, b: usize, pitch: f32) -> bool {
    if b - a < 2 {
        return false;
    }
    if inside[b - 1].y - inside[b].y > TABLE_LABEL_BELOW * pitch {
        return false;
    }
    let limit = TABLE_LABEL_GAP * pitch;
    (a + 1..b).all(|index| label_row(&rows[index]))
        && (a..b).all(|index| inside[index].y - inside[index + 1].y <= limit)
}

/// True when the row populates its first cell alone, a cell one column
/// wide: the section label of a statement, not a caption spanning the
/// columns.
fn label_row(row: &[Cell]) -> bool {
    let Some((first, rest)) = row.split_first() else {
        return false;
    };
    first.colspan == 1 && inked_cell(first) && rest.iter().all(|cell| !inked_cell(cell))
}

/// The lanes a set of lines leaves between their ink, left to right, a lane
/// being at least `min_gap` wide.
fn lanes_of(groups: &[Group], min_gap: f32) -> Vec<std::ops::Range<f32>> {
    let mut occupied: Vec<std::ops::Range<f32>> = Vec::new();
    for group in groups {
        for span in group.spans.iter().filter(|span| !blank(&span.text)) {
            add_ink(
                &mut occupied,
                span.x.min(span.end_x)..span.x.max(span.end_x),
            );
        }
    }
    ink_gaps(&occupied, min_gap)
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
    let mut inked_end: Option<f32> = None;
    for (position, &span) in group.spans.iter().enumerate() {
        // A whitespace-only span that sits in the columns claims like any
        // other, so a cell keeps its spacing; one running outside them —
        // a producer's padding past the grid's edge — paints nothing and
        // is skipped rather than disqualifying the whole row.
        let whitespace = blank(&span.text);
        let lo = span.x.min(span.end_x);
        let hi = span.x.max(span.end_x);
        // A span starting a hair left of the first column belongs to it: a
        // border rule is often drawn just inside the text's left edge.
        let Some(start) = columns
            .iter()
            .rposition(|column| column.start <= lo)
            .or_else(|| {
                (!whitespace && columns[0].start - lo <= RULING_SNAP_TOLERANCE).then_some(0)
            })
        else {
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
        // An inked span opening a cell a hair before a boundary and
        // crossing it belongs to the column beyond: an inferred vertical
        // lands a few points inside a floating currency sign, and a drawn
        // one is never painted through a glyph. A span ending before the
        // boundary keeps its column, so a right-aligned digit stays where
        // it is; a whitespace span keeps its column too, since moved, its
        // start would stand in for the next cell's; and a span within
        // [`CELL_GAP`] of the ink before it continues that ink's cell: text
        // set glyph by glyph straddles a rule glyph by glyph, a sentence
        // spanning the columns crosses a rule at a word gap, and either is
        // one cell over the columns it crosses.
        let crossing = start + 1 < columns.len() && hi > columns[start].end;
        let opens_cell = inked_end.is_none_or(|end| lo - end > CELL_GAP * span.size);
        let start = if crossing
            && !whitespace
            && opens_cell
            && columns[start].end - lo <= RULING_SNAP_TOLERANCE
        {
            start + 1
        } else {
            start
        };
        if !whitespace {
            inked_end = Some(hi);
        }
        let end = columns
            .iter()
            .rposition(|column| column.start <= hi)
            .map_or(start, |end| end.max(start));
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

/// Rows at the top of a lane band that are its title, not its header: set
/// in a heading size and populating at most half the cells of the row under
/// them. A title standing over two side-by-side grids populates one cell
/// per grid and stands a row's pitch above the header, so it passes every
/// row gate; it belongs to the prose above, where the heading pass reads
/// it. A header set larger than its body still fills most of its columns,
/// an empty label column aside, and stays the header.
fn title_rows(groups: &[Group], band: &TableBand, stats: &SizeStats) -> usize {
    let mut count = 0;
    while band.rows.len() - count > TABLE_MIN_ROWS {
        let group = &groups[band.span.start + count];
        if stats.level(assembled(group).rank_size).is_none() {
            break;
        }
        if 2 * populated_cells(&band.rows[count]) > populated_cells(&band.rows[count + 1]) {
            break;
        }
        count += 1;
    }
    count
}

fn populated_cells(row: &[Cell]) -> usize {
    row.iter().filter(|cell| cell.line.is_some()).count()
}

/// Currency signs written ahead of their amount in every locale, which a
/// producer sets left-aligned in a column of their own ahead of the
/// right-aligned amounts they belong to. The euro follows its amount in
/// most of Europe ("7 723 €") and stays where the page put it.
const AMOUNT_SIGNS: [char; 3] = ['$', '£', '¥'];
/// Closers a producer sets at a fixed position after the amount: the ")"
/// of a negative "(1,234" and the "%" of a rate.
const AMOUNT_CLOSERS: [char; 2] = [')', '%'];

/// Rejoins amounts their producer split across cells. A sign standing alone
/// in a cell or trailing the amount to its left moves onto the amount to
/// its right, as "$1,824"; a closer opening a cell moves onto the amount to
/// its left, as "(1,234)" and "12%"; and inside a cell the space before a
/// closer goes. A column that held nothing but signs is then blank in every
/// row, and is no column. The Markdown adapter calls this on the rows it
/// renders; the layout itself keeps every token where the page put it, so
/// the Text adapter reads the page as written.
pub(crate) fn tidy_amounts(rows: &mut [Vec<Cell>]) {
    let mut emptied: Vec<usize> = Vec::new();
    for row in rows.iter_mut() {
        let mut starts = Vec::with_capacity(row.len());
        let mut column = 0usize;
        for cell in row.iter() {
            starts.push(column);
            column += cell.colspan as usize;
        }
        let filled: Vec<usize> = (0..row.len())
            .filter(|index| row[*index].line.is_some())
            .collect();
        for (k, &index) in filled.iter().enumerate() {
            if k > 0 {
                let left = filled[k - 1];
                if let Some(closer) = opening_closer(&row[index]) {
                    if ends_with_digit(&row[left]) {
                        if strip_leading(&mut row[index], closer) {
                            emptied.push(starts[index]);
                        }
                        append_char(&mut row[left], closer);
                    }
                }
            }
            if k + 1 < filled.len() {
                let right = filled[k + 1];
                if let Some(sign) = trailing_sign(&row[index]) {
                    if opens_amount(&row[right]) {
                        if strip_trailing(&mut row[index], sign) {
                            emptied.push(starts[index]);
                        }
                        prepend_char(&mut row[right], sign);
                    }
                }
            }
            close_gaps(&mut row[index]);
            close_sign_gap(&mut row[index]);
        }
    }
    emptied.sort_unstable();
    emptied.dedup();
    for column in emptied.into_iter().rev() {
        if column_is_blank(rows, column) {
            remove_column(rows, column);
        }
    }
}

fn cell_text(cell: &Cell) -> String {
    cell.line.as_ref().map(line_text).unwrap_or_default()
}

/// The closer a cell opens with, standing alone or a space ahead of the rest.
fn opening_closer(cell: &Cell) -> Option<char> {
    let text = cell_text(cell);
    let text = text.trim_start();
    AMOUNT_CLOSERS.into_iter().find(|closer| {
        text.strip_prefix(*closer)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

/// The sign a cell ends with, standing alone or a space after the rest.
fn trailing_sign(cell: &Cell) -> Option<char> {
    let text = cell_text(cell);
    let text = text.trim_end();
    AMOUNT_SIGNS.into_iter().find(|sign| {
        text.strip_suffix(*sign)
            .is_some_and(|rest| rest.is_empty() || rest.ends_with(' '))
    })
}

fn ends_with_digit(cell: &Cell) -> bool {
    cell_text(cell)
        .trim_end()
        .ends_with(|c: char| c.is_ascii_digit())
}

/// True when the cell reads as an amount: a digit, an opening parenthesis,
/// or a dash standing for nil.
fn opens_amount(cell: &Cell) -> bool {
    cell_text(cell).trim_start().starts_with(|c: char| {
        c.is_ascii_digit() || matches!(c, '(' | '-' | '\u{2013}' | '\u{2014}')
    })
}

/// Takes `token` and the space after it off the front of the cell's text.
/// True when the token was all the cell held, and the cell is now empty.
fn strip_leading(cell: &mut Cell, token: char) -> bool {
    let Some(line) = cell.line.as_mut() else {
        return false;
    };
    let Some(index) = line.inlines.iter().position(|inline| !blank(&inline.text)) else {
        return false;
    };
    let text = line.inlines[index].text.trim_start();
    let rest = text
        .strip_prefix(token)
        .unwrap_or(text)
        .trim_start()
        .to_string();
    if !rest.is_empty() {
        line.inlines[index].text = rest;
        return false;
    }
    line.inlines.remove(index);
    if line.inlines.iter().any(|inline| !blank(&inline.text)) {
        return false;
    }
    cell.line = None;
    true
}

/// Takes `token` and the space before it off the end of the cell's text.
/// True when the token was all the cell held, and the cell is now empty.
fn strip_trailing(cell: &mut Cell, token: char) -> bool {
    let Some(line) = cell.line.as_mut() else {
        return false;
    };
    let Some(index) = line.inlines.iter().rposition(|inline| !blank(&inline.text)) else {
        return false;
    };
    let text = line.inlines[index].text.trim_end();
    let rest = text
        .strip_suffix(token)
        .unwrap_or(text)
        .trim_end()
        .to_string();
    if !rest.is_empty() {
        line.inlines[index].text = rest;
        return false;
    }
    line.inlines.remove(index);
    if line.inlines.iter().any(|inline| !blank(&inline.text)) {
        return false;
    }
    cell.line = None;
    true
}

fn prepend_char(cell: &mut Cell, token: char) {
    let Some(line) = cell.line.as_mut() else {
        return;
    };
    let Some(inline) = line.inlines.iter_mut().find(|inline| !blank(&inline.text)) else {
        return;
    };
    inline.text = format!("{token}{}", inline.text.trim_start());
}

fn append_char(cell: &mut Cell, token: char) {
    let Some(line) = cell.line.as_mut() else {
        return;
    };
    let Some(inline) = line.inlines.iter_mut().rfind(|inline| !blank(&inline.text)) else {
        return;
    };
    let kept = inline.text.trim_end().len();
    inline.text.truncate(kept);
    inline.text.push(token);
}

/// Closes the space a producer set between an amount and its closer, so
/// "(1,234 )" and "12 %" read "(1,234)" and "12%".
fn close_gaps(cell: &mut Cell) {
    let Some(line) = cell.line.as_mut() else {
        return;
    };
    for inline in &mut line.inlines {
        if !inline.text.contains(' ') {
            continue;
        }
        let mut out = String::with_capacity(inline.text.len());
        for c in inline.text.chars() {
            if AMOUNT_CLOSERS.contains(&c) {
                let kept = out.trim_end();
                if kept.len() < out.len() && kept.ends_with(|d: char| d.is_ascii_digit()) {
                    out.truncate(kept.len());
                }
            }
            out.push(c);
        }
        inline.text = out;
    }
}

/// Closes the padding a producer set between a sign opening a cell and the
/// amount after it, so "$    1,414.00" reads "$1,414.00", whether the sign
/// and the amount share an inline or the padding stands in inlines of its
/// own between them.
fn close_sign_gap(cell: &mut Cell) {
    let Some(line) = cell.line.as_mut() else {
        return;
    };
    let Some(first) = line.inlines.iter().position(|inline| !blank(&inline.text)) else {
        return;
    };
    let text = line.inlines[first].text.trim_start();
    let mut chars = text.chars();
    let Some(sign) = chars.next().filter(|c| AMOUNT_SIGNS.contains(c)) else {
        return;
    };
    let rest = chars.as_str();
    let amount = rest.trim_start();
    if amount.starts_with(|c: char| c.is_ascii_digit()) {
        if amount.len() < rest.len() {
            line.inlines[first].text = format!("{sign}{amount}");
        }
        return;
    }
    if !amount.is_empty() {
        return;
    }
    let Some(next) = line
        .inlines
        .iter()
        .skip(first + 1)
        .position(|inline| !blank(&inline.text))
        .map(|offset| first + 1 + offset)
    else {
        return;
    };
    let amount = line.inlines[next].text.trim_start();
    if !amount.starts_with(|c: char| c.is_ascii_digit()) {
        return;
    }
    line.inlines[next].text = format!("{sign}{amount}");
    line.inlines.drain(first..next);
}

/// True when no row puts ink in `column`: every cell starting there has no
/// line or holds whitespace alone, and a cell reaching over it from the
/// left carries its text elsewhere.
fn column_is_blank(rows: &[Vec<Cell>], column: usize) -> bool {
    rows.iter().all(|row| {
        let mut start = 0usize;
        for cell in row {
            let end = start + cell.colspan as usize;
            if start == column {
                return !inked_cell(cell);
            }
            if start < column && column < end {
                return true;
            }
            start = end;
        }
        true
    })
}

/// Takes `column` out of every row: a cell standing in it alone goes, a
/// cell reaching over it narrows by one.
fn remove_column(rows: &mut [Vec<Cell>], column: usize) {
    for row in rows.iter_mut() {
        let mut start = 0usize;
        for index in 0..row.len() {
            let width = row[index].colspan as usize;
            if start == column && width == 1 {
                row.remove(index);
                break;
            }
            if start <= column && column < start + width {
                row[index].colspan -= 1;
                break;
            }
            start += width;
        }
    }
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
        if blank(&span.text) {
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
    let mut first_bucket: Option<(f32, i32)> = None;
    let mut mixed = false;
    for span in spans {
        let spaced = prev_end.is_some_and(|end| span.x - end > WORD_GAP * prev_size.max(span.size));
        push_span(&mut inlines, span, spaced, capacity);
        prev_end = Some(span.end_x);
        prev_size = span.size;
        // A whitespace-only span has no visible size, so it has no vote in
        // the line's size rank: a producer's stray body-size separator on a
        // heading's baseline must not fold the heading into the paragraph.
        if blank(&span.text) {
            continue;
        }
        match first_bucket {
            None => first_bucket = Some((span.size, half_points(span.size))),
            Some((_, bucket)) if bucket != half_points(span.size) => mixed = true,
            _ => {}
        }
    }
    let rank_size = match (first_bucket, mixed) {
        (None, _) => size,
        (Some((first, _)), false) => first,
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
///
/// Covers ISO 32000-1 §14.8.2.5.
fn push_span(inlines: &mut Vec<Inline>, span: &TextSpan, spaced: bool, capacity: usize) {
    let code = in_code(span);
    if let Some(last) = inlines.last_mut() {
        let already_spaced =
            last.text.ends_with(char::is_whitespace) || span.text.starts_with(char::is_whitespace);
        if spaced && !already_spaced {
            last.text.push(' ');
        }
        if last.bold == span.bold && last.italic == span.italic && last.code == code {
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
        code,
    });
}

/// Whether the span was shown inside a `Code` structure element (§14.8.4.4):
/// computer code, which the Markdown adapter sets as inline code. The other
/// inline-level elements (Span, Quote, Reference, BibEntry, Annot, Link,
/// Note, Ruby, Warichu) leave their text flowing as shown.
///
/// Covers ISO 32000-1 §14.8.4.4.
fn in_code(span: &TextSpan) -> bool {
    span.structure.as_ref().is_some_and(|structure| {
        structure
            .path
            .iter()
            .any(|element| element.standard_type == StandardType::Code)
    })
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
    let raw = flows(spans);
    // The stream's own flow extents, kept before any reordering: whether
    // the producer interleaved the columns is a question about the stream.
    let raw_extents: Vec<(f32, f32)> = raw
        .iter()
        .map(|flow| {
            (
                flow.iter().map(|s| s.bbox.x0).fold(f32::MAX, f32::min),
                flow.iter().map(|s| s.bbox.x1).fold(f32::MIN, f32::max),
            )
        })
        .collect();
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
    // The assignment is a fact about each flow, computed in stream order so
    // the stream-order gates below can pair it with `raw_extents`; when the
    // visual reorder moves the flows, it moves with them.
    let stream_assignment: Vec<Option<usize>> = if grids.is_empty() {
        vec![None; raw.len()]
    } else {
        raw.iter().map(|flow| grid_of(flow)).collect()
    };
    // The cheap pre-gate for the page pass: the final gate needs the
    // stream to alternate across the gutter at least twice, and flows that
    // never alternate across even the text's midline cannot. Everything
    // here derives from the per-flow extents in stream order, so an
    // ordinary page pays no allocation and no page-wide grouping.
    let alternates = raw.len() > 1 && {
        let (mut x_min, mut x_max) = (f32::MAX, f32::MIN);
        for (extent, assigned) in raw_extents.iter().zip(&stream_assignment) {
            if assigned.is_none() {
                x_min = x_min.min(extent.0);
                x_max = x_max.max(extent.1);
            }
        }
        let mid = (x_min + x_max) / 2.0;
        let mut lanes = raw_extents
            .iter()
            .zip(&stream_assignment)
            .filter(|(_, assigned)| assigned.is_none())
            .filter_map(|((x0, x1), _)| {
                if *x1 <= mid {
                    Some(true)
                } else if *x0 >= mid {
                    Some(false)
                } else {
                    None
                }
            });
        let mut switches = 0usize;
        if let Some(mut lane) = lanes.next() {
            for next in lanes {
                if next != lane {
                    switches += 1;
                    lane = next;
                }
            }
        }
        switches >= 2
    };
    let (ordered, assignment) = match visual_flow_order(&raw) {
        Some(order) => {
            let mut slots: Vec<Option<Vec<&TextSpan>>> = raw.into_iter().map(Some).collect();
            let ordered: Vec<Vec<&TextSpan>> = order
                .iter()
                .map(|&i| slots[i].take().expect("each flow placed once"))
                .collect();
            let assignment: Vec<Option<usize>> =
                order.iter().map(|&i| stream_assignment[i]).collect();
            (ordered, assignment)
        }
        None => (raw, stream_assignment),
    };
    let mut merged: Vec<Vec<&TextSpan>> = vec![Vec::new(); grids.len()];
    for (flow, assigned) in ordered.iter().zip(&assignment) {
        if let Some(grid) = assigned {
            merged[*grid].extend(flow.iter().copied());
        }
    }
    // The page's loose text, all of it at once: a page with a true gutter
    // reads column-major across every flow — a report writing its columns
    // as interleaved paragraphs still reads left column first — while
    // stream order survives inside each lane. Without a page gutter each
    // flow splits on its own, which is what keeps a designed page's
    // side-by-side blocks apart.
    let loose: Vec<&TextSpan> = if alternates {
        ordered
            .iter()
            .zip(&assignment)
            .filter(|(_, assigned)| assigned.is_none())
            .flat_map(|(flow, _)| flow.iter().copied())
            .collect()
    } else {
        Vec::new()
    };
    let page_bands = (alternates && loose.len() >= COLUMN_MIN_SPANS)
        .then(|| {
            let (x_min, x_max) = x_bounds(&loose);
            let width = x_max - x_min;
            if !width.is_finite() || width <= 0.0 {
                return None;
            }
            let lines = line_groups(&loose);
            let (bands, cut) = split_at_gutter(&lines, x_min, width)?;
            // An ordered stream writes its left column whole and crosses
            // the gutter once; only flows that alternate across it need
            // the page's lanes imposed. One switch is order, several are
            // interleaving.
            let mut lanes: Vec<bool> = Vec::new();
            for (x0, x1) in &raw_extents {
                if *x1 <= cut {
                    lanes.push(true);
                } else if *x0 >= cut {
                    lanes.push(false);
                }
            }
            let switches = lanes.windows(2).filter(|pair| pair[0] != pair[1]).count();
            (switches >= 2).then_some(bands)
        })
        .flatten();

    let mut out = Vec::new();
    if let Some(bands) = page_bands {
        out.extend(bands);
        // A band's empty sides go before the grids slot in by height: an
        // empty segment has no height and would match any grid's top.
        out.retain(|segment| !segment.spans.is_empty());
        // Each grid's segment slots in by height: before the first band
        // that starts below the grid's top.
        for spans in merged {
            if spans.is_empty() {
                continue;
            }
            let top = spans.iter().map(|s| s.bbox.y1).fold(f32::MIN, f32::max);
            let position = out
                .iter()
                .position(|segment: &Segment| {
                    segment
                        .spans
                        .iter()
                        .map(|s| s.bbox.y1)
                        .fold(f32::MIN, f32::max)
                        < top
                })
                .unwrap_or(out.len());
            out.insert(position, Segment::ungrouped(spans));
        }
        return out;
    }

    let mut emitted = vec![false; grids.len()];
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
fn sequential_groups<'s>(spans: impl IntoIterator<Item = &'s TextSpan>) -> Vec<Group<'s>> {
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
///
/// Returns the permutation for the caller to apply — the caller carries
/// per-flow state (grid assignments) that must move with the flows — and
/// `None` when the stream's order stands.
fn visual_flow_order(flows: &[Vec<&TextSpan>]) -> Option<Vec<usize>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    struct Extent {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        chars: usize,
    }

    /// Whether `a` lies entirely above `b` while sharing at least
    /// [`VISUAL_ORDER_MIN_X_OVERLAP`] of the narrower one's width.
    fn lies_above(a: &Extent, b: &Extent) -> bool {
        let overlap = a.right.min(b.right) - a.left.max(b.left);
        let narrower = (a.right - a.left).min(b.right - b.left);
        narrower > 0.0 && overlap >= VISUAL_ORDER_MIN_X_OVERLAP * narrower && a.bottom > b.top
    }

    let n = flows.len();
    if !(2..=VISUAL_ORDER_MAX_FLOWS).contains(&n) {
        return None;
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
        return None;
    }
    // A stream whose later flows never lie above an earlier one keeps its
    // order under the sort below; the common page leaves here.
    let ordered = (0..n).all(|a| {
        (a + 1..n).all(|b| {
            extents[a].chars < VISUAL_ORDER_MIN_CHARS
                || extents[b].chars < VISUAL_ORDER_MIN_CHARS
                || !lies_above(&extents[b], &extents[a])
        })
    });
    if ordered {
        return None;
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
            if lies_above(&extents[a], &extents[b]) {
                above[a].push(b);
                blockers[b] += 1;
            }
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> =
        (0..n).filter(|&i| blockers[i] == 0).map(Reverse).collect();
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
        return None;
    }
    if order
        .iter()
        .enumerate()
        .all(|(position, &flow)| position == flow)
    {
        return None;
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
        return None;
    }
    Some(order)
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
        Some((bands, _)) => bands,
        None => vec![Segment {
            spans,
            groups: Some(lines),
        }],
    }
}

/// The bands of a flow that has a gutter, or `None` when it has none and
/// reads whole. `lines` are the flow's line groups, which the caller keeps
/// for the block pass when the flow stays whole.
fn split_at_gutter<'s>(
    lines: &[Group<'s>],
    x_min: f32,
    width: f32,
) -> Option<(Vec<Segment<'s>>, f32)> {
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
    // Each line splits once at the cut and both halves keep their baseline
    // identity, so the bands below carry prebuilt groups and no segment
    // regroups its spans.
    let mut left_groups: Vec<Group<'s>> = Vec::new();
    let mut right_groups: Vec<Group<'s>> = Vec::new();
    for line in &columns {
        let (l, r): (Vec<&TextSpan>, Vec<&TextSpan>) =
            line.spans.iter().partition(|s| s.x.max(s.end_x) <= cut);
        if !l.is_empty() {
            left_groups.push(Group {
                y: line.y,
                size: line.size,
                spans: l,
            });
        }
        if !r.is_empty() {
            right_groups.push(Group {
                y: line.y,
                size: line.size,
                spans: r,
            });
        }
    }
    let left: Vec<&TextSpan> = left_groups
        .iter()
        .flat_map(|g| g.spans.iter().copied())
        .collect();
    let right: Vec<&TextSpan> = right_groups
        .iter()
        .flat_map(|g| g.spans.iter().copied())
        .collect();
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
        push_band(&left_groups, &right_groups, top, sep_y, &mut out);
        out.push(Segment::ungrouped(
            crossing
                .iter()
                .filter(|line| line.y == sep_y)
                .flat_map(|line| line.spans.iter().copied())
                .collect(),
        ));
        top = sep_y;
    }
    push_band(
        &left_groups,
        &right_groups,
        top,
        f32::NEG_INFINITY,
        &mut out,
    );
    Some((out, cut))
}

/// Pushes one band's columns — the spans with baseline in `(bottom, top]` —
/// left side first. A column half is prose: its one gutter lane belongs to
/// the page, not to anything inside the column.
fn push_band<'s>(
    left: &[Group<'s>],
    right: &[Group<'s>],
    top: f32,
    bottom: f32,
    out: &mut Vec<Segment<'s>>,
) {
    for side in [left, right] {
        let groups: Vec<Group<'s>> = side
            .iter()
            .filter(|group| group.y <= top && group.y > bottom)
            .map(|group| Group {
                y: group.y,
                size: group.size,
                spans: group.spans.clone(),
            })
            .collect();
        let spans: Vec<&TextSpan> = groups
            .iter()
            .flat_map(|group| group.spans.iter().copied())
            .collect();
        out.push(Segment {
            spans,
            groups: Some(groups),
        });
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

    /// A synthetic span for driving the segmentation machinery directly,
    /// where a content-stream fixture would be hostage to every upstream
    /// heuristic at once.
    fn span(text: &str, x: f32, end_x: f32, y: f32, size: f32) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            x,
            y,
            end_x,
            size,
            font: "F1".to_string(),
            font_name: String::new(),
            page: 0,
            bbox: pdfboss_core::Rect {
                x0: x,
                y0: y - 2.0,
                x1: end_x,
                y1: y + 8.0,
            },
            bold: false,
            italic: false,
            monospace: false,
            serif: false,
            rise: 0.0,
            vertical: false,
            invisible: false,
            color: None,
            underline: false,
            strikethrough: false,
            ascent: 8.0,
            descent: -2.0,
            highlight: false,
            highlight_color: None,
            artifact: None,
            structure: None,
            alt: None,
            lang: None,
            expansion: None,
        }
    }

    /// A synthetic horizontal ruling.
    fn hrule(y: f32, x0: f32, x1: f32) -> Ruling {
        Ruling {
            start: pdfboss_core::Point { x: x0, y },
            end: pdfboss_core::Point { x: x1, y },
            width: 0.5,
        }
    }

    /// A rule drawn in segments meeting end to end is one rule; the
    /// underlines of two column heads, a gutter apart, stay two.
    #[test]
    fn rule_segments_meeting_end_to_end_join() {
        let joined = joined_rules(vec![
            (732.8, 495.0, 537.8),
            (612.0, 100.0, 147.0),
            (732.8, 324.0, 495.0),
            (612.0, 10.0, 95.0),
            (732.8, 537.8, 576.0),
        ]);
        assert_eq!(
            joined,
            vec![
                (612.0, 10.0, 95.0),
                (612.0, 100.0, 147.0),
                (732.8, 324.0, 576.0)
            ]
        );
    }

    /// A page buried in stacked aligned rules — graph paper, a form's
    /// writing lines — skips open-ruled detection outright: no cluster of
    /// hundreds of rules is a table, and chewing through one costs a
    /// recursion per rule.
    #[test]
    fn a_rule_stack_past_the_cap_skips_open_ruled_detection() {
        let mut rulings: Vec<Ruling> = (0..597)
            .map(|i| hrule(100.0 + 0.5 * i as f32, 70.0, 430.0))
            .collect();
        rulings.extend([
            hrule(610.0, 70.0, 430.0),
            hrule(688.0, 70.0, 430.0),
            hrule(710.0, 70.0, 430.0),
        ]);
        let mut spans = vec![
            span("Added cation", 72.0, 150.0, 700.0, 10.0),
            span("Relative rates", 260.0, 340.0, 700.0, 10.0),
        ];
        for (row, y) in [676.0, 656.0, 636.0, 616.0].into_iter().enumerate() {
            spans.push(span(&format!("K{row}+"), 72.0, 110.0, y, 10.0));
            spans.push(span("slow", 260.0, 300.0, y, 10.0));
        }
        assert!(
            open_ruled_grids(&spans, &rulings, &[]).is_empty(),
            "a 600-rule stack is not table territory"
        );
    }

    /// A chart's plot grid: many aligned rules at near-identical gaps with
    /// data labels hugging some of them. No corner of it is a table, however
    /// well a subcluster's rules hug the labels.
    #[test]
    fn an_even_rule_stack_reads_as_a_plot_grid_not_a_table() {
        let ys = [
            582.6, 602.0, 622.3, 641.7, 662.0, 681.4, 701.7, 722.0, 741.4,
        ];
        let rulings: Vec<Ruling> = ys.iter().map(|&y| hrule(y, 125.4, 488.8)).collect();
        let mut spans = Vec::new();
        for (y, xs) in [
            (735.0, [180.0, 280.0, 380.0]),
            (712.0, [160.0, 260.0, 360.0]),
            (676.0, [200.0, 300.0, 400.0]),
            (668.0, [220.0, 320.0, 420.0]),
        ] {
            for x in xs {
                spans.push(span("68", x, x + 14.0, y, 8.0));
            }
        }
        assert!(
            open_ruled_grids(&spans, &rulings, &[]).is_empty(),
            "identical bands are drawn guides, not rows"
        );
    }

    /// One block of single-span lines, top line at `top`, stepping down 12.
    fn block(prefix: &str, x: f32, end_x: f32, top: f32, lines: usize, spans: &mut Vec<TextSpan>) {
        for i in 0..lines {
            let text = format!("{prefix} body line {i} of text");
            spans.push(span(&text, x, end_x, top - 12.0 * i as f32, 10.0));
        }
    }

    /// A two-by-two drawn grid at the page bottom and its four cell spans.
    fn bottom_grid(spans: &mut Vec<TextSpan>) -> RuledGrid {
        for (text, x, end_x, y) in [
            ("cell a1", 80.0, 170.0, 240.0),
            ("cell b1", 190.0, 280.0, 240.0),
            ("cell a2", 80.0, 170.0, 220.0),
            ("cell b2", 190.0, 280.0, 220.0),
        ] {
            spans.push(span(text, x, end_x, y, 10.0));
        }
        RuledGrid {
            xs: vec![76.0, 180.0, 284.0],
            ys: vec![186.0, 254.0],
            reach: vec![76.0..284.0, 76.0..284.0],
            boxed: true,
            open: false,
        }
    }

    /// A page whose columns interleave across the gutter (the page-lane
    /// path), with a full-width title above them and a drawn grid at the
    /// bottom, its cell text written first in the stream. The title's band
    /// stripe leaves empty column segments above it, and the grid must
    /// still slot in at the bottom — below the title and both columns —
    /// not before them.
    #[test]
    fn a_bottom_grid_slots_below_the_title_band() {
        let mut spans = Vec::new();
        let grid = bottom_grid(&mut spans);
        spans.push(span(
            "The annual report of the society",
            72.0,
            288.0,
            740.0,
            10.0,
        ));
        block("left one", 72.0, 160.0, 700.0, 10, &mut spans);
        block("right one", 200.0, 288.0, 628.0, 10, &mut spans);
        block("left two", 72.0, 160.0, 556.0, 10, &mut spans);
        block("right two", 200.0, 288.0, 484.0, 10, &mut spans);
        block("left three", 72.0, 160.0, 412.0, 10, &mut spans);
        let segments = segments_with_grids(&spans, std::slice::from_ref(&grid));
        assert!(
            segments[0]
                .spans
                .iter()
                .any(|s| s.text.starts_with("The annual report")),
            "the title reads first"
        );
        let grid_at = segments
            .iter()
            .position(|seg| seg.spans.iter().any(|s| s.text == "cell a1"))
            .expect("the grid's text is somewhere");
        assert_eq!(
            grid_at,
            segments.len() - 1,
            "the bottom grid reads last, not hoisted above the title"
        );
    }

    /// The interleaving gate reads the stream: each flow's extent pairs
    /// with that same flow's grid assignment even after the visual reorder
    /// has moved a grid's flow (here written first, sitting last). With
    /// the pairing shifted, the last loose flow drops out of the lane
    /// sequence and the page loses its column-major read.
    #[test]
    fn a_reordered_grid_flow_does_not_shift_the_lane_gate() {
        let mut spans = Vec::new();
        let grid = bottom_grid(&mut spans);
        block("right one", 200.0, 288.0, 700.0, 10, &mut spans);
        block("right two", 200.0, 288.0, 628.0, 10, &mut spans);
        block("left one", 72.0, 160.0, 556.0, 16, &mut spans);
        block("right three", 200.0, 288.0, 412.0, 10, &mut spans);
        let segments = segments_with_grids(&spans, std::slice::from_ref(&grid));
        let right_column = segments
            .iter()
            .find(|seg| {
                seg.spans
                    .iter()
                    .any(|s| s.text.starts_with("right one body line 0"))
            })
            .expect("the right column is somewhere");
        assert!(
            right_column
                .spans
                .iter()
                .any(|s| s.text.starts_with("right three")),
            "the right column reads whole, top block to bottom block"
        );
    }

    /// A lone top line set in the body's own type, standing well over a
    /// pitch above a paragraph and far short of the measure, is the page's
    /// title even though nothing in its type says so.
    #[test]
    fn an_isolated_body_size_top_line_becomes_the_page_title() {
        let mut spans = vec![span("Print against Digital", 54.0, 140.0, 714.0, 11.5)];
        for y in [682.0, 664.7, 647.4] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        let layout = page_layout(&spans, ReadingOrder::Content);
        let Block::Heading { level, lines, .. } = &layout.blocks[0] else {
            panic!(
                "the isolated top line is the page's title, got {:?}",
                layout.blocks[0]
            );
        };
        assert_eq!(*level, 1);
        assert_eq!(line_text(&lines[0]).trim(), "Print against Digital");
    }

    /// A page whose stray far-down lines (a widowed word, a footer) skew
    /// the median step folds everything into one paragraph block; the
    /// title is carved out of that block all the same.
    #[test]
    fn a_title_folded_into_the_page_block_still_promotes() {
        let mut spans = vec![span("Print against Digital", 54.0, 140.0, 714.0, 11.5)];
        for y in [682.0, 664.7] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        spans.push(span("format.", 54.0, 96.0, 390.0, 11.5));
        spans.push(span("Online Survey", 482.0, 558.0, 43.0, 8.0));
        let layout = page_layout(&spans, ReadingOrder::Content);
        let Block::Heading { lines, .. } = &layout.blocks[0] else {
            panic!("the folded title is carved out, got {:?}", layout.blocks[0]);
        };
        assert_eq!(line_text(&lines[0]).trim(), "Print against Digital");
        let Block::Paragraph { lines, .. } = &layout.blocks[1] else {
            panic!("the body stays a paragraph, got {:?}", layout.blocks[1]);
        };
        assert!(line_text(&lines[0]).starts_with("why readers"));
    }

    /// A top line set smaller than the body is a running header, however
    /// isolated it stands.
    #[test]
    fn a_top_line_smaller_than_body_stays_a_running_header() {
        let mut spans = vec![span("Print against Digital", 54.0, 140.0, 714.0, 8.0)];
        for y in [682.0, 664.7, 647.4] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        let layout = page_layout(&spans, ReadingOrder::Content);
        assert!(
            !layout
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Heading { .. })),
            "a sub-body top line never promotes"
        );
    }

    /// An isolated top line carrying a digit is a folio or a running
    /// header, not a title.
    #[test]
    fn a_top_line_with_a_digit_stays_prose() {
        let mut spans = vec![span("16 Face Your World", 54.0, 140.0, 714.0, 11.5)];
        for y in [682.0, 664.7, 647.4] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        let layout = page_layout(&spans, ReadingOrder::Content);
        assert!(
            !layout
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Heading { .. })),
            "a numbered top line never promotes"
        );
    }

    /// A full-measure top line is prose, however isolated: titles stop
    /// short of the margin.
    #[test]
    fn a_full_measure_top_line_stays_prose() {
        let mut spans = vec![span(
            "As seen in this chart of responses we should adopt the format",
            54.0,
            558.0,
            714.0,
            11.5,
        )];
        for y in [682.0, 664.7, 647.4] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        let layout = page_layout(&spans, ReadingOrder::Content);
        assert!(
            !layout
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Heading { .. })),
            "a full-measure top line never promotes"
        );
    }

    /// A page whose type already announced a heading needs no promoted
    /// title: typographic evidence outranks the structural read.
    #[test]
    fn a_page_with_a_sized_heading_promotes_nothing() {
        let mut spans = vec![span("Print against Digital", 54.0, 140.0, 714.0, 11.5)];
        for y in [682.0, 664.7, 647.4] {
            spans.push(span(
                "why readers pick one format over the other and stay with it",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        spans.push(span("Results", 54.0, 120.0, 600.0, 16.0));
        for y in [566.0, 548.7] {
            spans.push(span(
                "what the numbers in the returned forms actually said",
                54.0,
                558.0,
                y,
                11.5,
            ));
        }
        let layout = page_layout(&spans, ReadingOrder::Content);
        let headings: Vec<&Block> = layout
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .collect();
        assert_eq!(headings.len(), 1, "only the sized heading stands");
        assert!(
            matches!(&layout.blocks[0], Block::Paragraph { .. }),
            "the top line stays prose beside real typographic headings"
        );
    }

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
                let Ok((mut spans, rulings, _)) = pdfboss_text::extract_spans_and_rulings_reporting(
                    &doc,
                    &page,
                    ReadingOrder::Content,
                ) else {
                    continue;
                };
                crate::retain_spans_on_page(&mut spans, &page);
                let crop = page.crop_box;
                let grids = ruled_grids(&rulings, RULING_SNAP_TOLERANCE);
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
                        b.x0,
                        b.y0,
                        b.x1,
                        b.y1
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
                        .map(|c| {
                            if c.is_ascii_graphic() || c == ' ' {
                                c
                            } else {
                                '?'
                            }
                        })
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
            pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page, ReadingOrder::Content)
                .unwrap();
        println!(
            "rulings: {} (report complete: {})",
            rulings.len(),
            report.is_complete()
        );
        for r in rulings.iter().take(60) {
            println!(
                "  ({:7.1},{:7.1}) -> ({:7.1},{:7.1}) w={:.2}",
                r.start.x, r.start.y, r.end.x, r.end.y, r.width
            );
        }
        let snap = ruling_snap(&spans, &rulings);
        let grids = ruled_grids(&rulings, snap);
        println!("grids: {} (snap {snap})", grids.len());
        for grid in &grids {
            println!(
                "  xs {:?} ys {} boxed {}",
                grid.xs,
                grid.ys.len(),
                grid.boxed
            );
        }
        let open = open_ruled_grids(&spans, &rulings, &grids);
        println!("open grids: {}", open.len());
        for grid in &open {
            println!("  xs {:?} ys {:?}", grid.xs, grid.ys);
        }
        // The grid sets the layout itself works with: the stacks folded
        // into hulls for the segmentation, every grid for the claims, open
        // grids appended to both, topmost first.
        let mut hulls = stack_hulls(&grids, &spans, snap);
        hulls.extend(open.iter().cloned());
        hulls.sort_by(|a, b| b.ys[b.ys.len() - 1].total_cmp(&a.ys[a.ys.len() - 1]));
        let mut grids = grids;
        grids.extend(open);
        grids.sort_by(|a, b| b.ys[b.ys.len() - 1].total_cmp(&a.ys[a.ys.len() - 1]));
        println!("layout grids: {} in {} hulls", grids.len(), hulls.len());
        {
            let all: Vec<&TextSpan> = spans.iter().collect();
            let (x_min, x_max) = x_bounds(&all);
            let width = x_max - x_min;
            let lines = line_groups(&all);
            let scale = GUTTER_BINS as f32 / width;
            let mut coverage = [0usize; GUTTER_BINS];
            for line in &lines {
                let mut covered = [false; GUTTER_BINS];
                fill_bins(&mut covered, &line.spans, x_min, scale);
                for (count, hit) in coverage.iter_mut().zip(covered) {
                    *count += usize::from(hit);
                }
            }
            let allowed = (GUTTER_MAX_CROSSING * lines.len() as f32) as usize;
            let occupied: [bool; GUTTER_BINS] = std::array::from_fn(|bin| coverage[bin] > allowed);
            let gaps = wide_gaps(&occupied, scale);
            println!(
                "page-level: {} lines, allowed {}, gaps {:?} (band {:?})",
                lines.len(),
                allowed,
                gaps.iter()
                    .map(|g| {
                        let center = (g.start + g.end) as f32 / 2.0 / GUTTER_BINS as f32;
                        (g.start, g.end, (center * 100.0) as i32)
                    })
                    .collect::<Vec<_>>(),
                GUTTER_BAND
            );
            if let [gutter] = gaps.as_slice() {
                let cut = x_min + (gutter.start + gutter.end) as f32 / 2.0 / scale;
                let (crossing, columns): (Vec<&Group>, Vec<&Group>) =
                    lines.iter().partition(|line| {
                        let mut covered = [false; GUTTER_BINS];
                        fill_bins(&mut covered, &line.spans, x_min, scale);
                        covered[gutter.clone()].iter().any(|hit| *hit)
                    });
                let body: Vec<&TextSpan> = columns
                    .iter()
                    .flat_map(|line| line.spans.iter().copied())
                    .collect();
                let (left, right): (Vec<&TextSpan>, Vec<&TextSpan>) =
                    body.iter().partition(|s| s.x.max(s.end_x) <= cut);
                println!(
                    "  crossing {} columns {}; left shaped {} span {:.2} right shaped {} span {:.2}",
                    crossing.len(),
                    columns.len(),
                    column_shaped(&left),
                    x_span(&left) / width,
                    column_shaped(&right),
                    x_span(&right) / width,
                );
                let (left_lo, left_hi) = y_extent(&left);
                let (right_lo, right_hi) = y_extent(&right);
                let height = left_hi.max(right_hi) - left_lo.min(right_lo);
                println!(
                    "  heights: left {:.2} right {:.2} (min {})",
                    (left_hi - left_lo) / height,
                    (right_hi - right_lo) / height,
                    COLUMN_MIN_HEIGHT
                );
            }
        }
        for (index, segment) in segments_with_grids(&spans, &hulls).into_iter().enumerate() {
            let (x_lo, x_hi) = x_bounds(&segment.spans);
            let y_hi = segment.spans.iter().map(|s| s.y).fold(f32::MIN, f32::max);
            let y_lo = segment.spans.iter().map(|s| s.y).fold(f32::MAX, f32::min);
            let groups = segment.into_groups();
            let claims = grid_claims(&groups, &grids, &hulls, snap);
            println!(
                "segment {index}: {} groups, {} claims, x {x_lo:.0}..{x_hi:.0}, y {y_lo:.0}..{y_hi:.0}",
                groups.len(),
                claims.len()
            );
            for claim in &claims {
                println!(
                    "  claim {:?}: {} rows of {} columns",
                    claim.range,
                    claim.rows.len(),
                    claim.rows.first().map_or(0, |row| row.len())
                );
            }
            if std::env::var_os("PDFBOSS_PROBE_LINES").is_some() {
                for (gi, group) in groups.iter().enumerate() {
                    let (gx_lo, gx_hi) = x_bounds(&group.spans);
                    let LaneRun {
                        end: run_end,
                        lanes,
                    } = lane_runs(&groups, gi, gutter_min(&groups)).0;
                    let text: String = group
                        .spans
                        .iter()
                        .flat_map(|s| s.text.chars())
                        .take(50)
                        .collect();
                    println!(
                        "  line {gi:3}: y {:6.1} x {gx_lo:5.0}..{gx_hi:5.0} run {}..{} lanes {}  {:?}",
                        group.y,
                        gi,
                        run_end,
                        lanes.len(),
                        text
                    );
                }
                match table_band(&groups) {
                    Some(band) => println!(
                        "  table_band: span {:?}, {} rows of {} columns",
                        band.span,
                        band.rows.len(),
                        band.rows.first().map_or(0, |row| row.len())
                    ),
                    None => println!("  table_band: none"),
                }
            }
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
                let columns = lane_split_columns(
                    open_columns(&groups[lo..hi], grid),
                    &groups[stack_lines(&groups, grid, &hulls).unwrap_or(lo..hi)],
                );
                println!(
                    "  columns {:?} (drawn {:?}), header reach {}",
                    columns,
                    grid.columns(),
                    header_reach(&groups, lo, hi, grid, &grids, &columns)
                );
                if grid.open {
                    println!("  open reach {}", open_reach(&groups, lo, hi, grid, &grids));
                }
                for (gi, group) in groups[lo..hi].iter().enumerate() {
                    if table_row(group, &columns).is_none() {
                        let text: String = group
                            .spans
                            .iter()
                            .flat_map(|s| s.text.chars())
                            .take(60)
                            .collect();
                        println!("  table_row fails at line {}: {:?}", lo + gi, text);
                        for span in &group.spans {
                            println!(
                                "      span x {:7.1}..{:7.1} size {:4.1} {:?}",
                                span.x.min(span.end_x),
                                span.x.max(span.end_x),
                                span.size,
                                span.text
                            );
                        }
                    }
                }
            }
        }
        if std::env::var_os("PDFBOSS_PROBE_MD").is_some() {
            println!("---- markdown ----");
            println!(
                "{}",
                crate::extract_markdown(&doc, ReadingOrder::Content).unwrap()
            );
        }
        if std::env::var_os("PDFBOSS_PROBE_TEXT").is_some() {
            println!("---- text ----");
            println!(
                "{}",
                crate::extract_text(&doc, &page, ReadingOrder::Content).unwrap()
            );
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
                let Ok((mut spans, _)) =
                    pdfboss_text::extract_spans_reporting(&doc, &page, ReadingOrder::Content)
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
                        .map(|c| {
                            if c.is_ascii_graphic() || c == ' ' {
                                c
                            } else {
                                '?'
                            }
                        })
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

    /// [`lane_grid_content`] with a page footer far below it that populates
    /// two of the grid's columns — a form title and a page number — the way
    /// a 10-K page ends. The footer is a populated row a hundred points off
    /// the grid's pitch; the grid must survive it as prose below.
    pub(crate) fn grid_with_far_footer_content() -> String {
        let mut content = lane_grid_content();
        content.truncate(content.len() - "ET".len());
        content += "1 0 0 1 72 520 Tm (Form 10-K) Tj 1 0 0 1 430 520 Tm (41) Tj ET";
        content
    }

    /// Two three-column grids side by side under one title set in a
    /// heading size, the way a rate manual sets "Symbols" over its proposed
    /// and current tables. The title populates one cell per grid and stands
    /// a row's pitch above the header, so it passes every row gate.
    pub(crate) fn titled_side_by_side_grids_content() -> String {
        let mut content = String::from(
            "BT /F1 16 Tf 1 0 0 1 72 720 Tm (Symbols) Tj 1 0 0 1 330 720 Tm (Symbols) Tj /F1 10 Tf ",
        );
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [
                (0, 72.0),
                (1, 150.0),
                (2, 230.0),
                (3, 330.0),
                (4, 410.0),
                (5, 490.0),
            ] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// A financial statement's rows: a label, then each amount set as a
    /// left-aligned "$" in a column of its own and the digits far to its
    /// right, closer to the next "$" than to their own.
    pub(crate) fn currency_columns_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        let rows = [
            ("Gross", 700.0, ["1,824", "1,889", "1,978"]),
            ("Net", 680.0, ["1,702", "1,777", "1,840"]),
            ("Paid", 660.0, ["122", "112", "138"]),
        ];
        for (label, y, amounts) in rows {
            content += &format!("1 0 0 1 72 {y} Tm ({label}) Tj ");
            for (amount, (sign_x, amount_x)) in
                amounts
                    .iter()
                    .zip([(250.0, 275.0), (303.0, 330.0), (358.0, 385.0)])
            {
                content += &format!(
                    "1 0 0 1 {sign_x} {y} Tm ($) Tj 1 0 0 1 {amount_x} {y} Tm ({amount}) Tj "
                );
            }
        }
        content += "ET";
        content
    }

    /// Negative amounts and a rate whose closers stand apart: a ")" a lane
    /// away from its "(1,234", a ")" a word gap after its "(5", and a "%"
    /// in the ")" column after "12".
    pub(crate) fn split_closers_content() -> String {
        String::from(
            "BT /F1 10 Tf \
             1 0 0 1 72 700 Tm (Loss) Tj 1 0 0 1 275 700 Tm (\\(1,234) Tj 1 0 0 1 312 700 Tm (\\)) Tj \
             1 0 0 1 400 700 Tm (\\(5) Tj 1 0 0 1 412 700 Tm (\\)) Tj \
             1 0 0 1 72 680 Tm (Gain) Tj 1 0 0 1 275 680 Tm (1,889) Tj 1 0 0 1 400 680 Tm (7) Tj \
             1 0 0 1 72 660 Tm (Rate) Tj 1 0 0 1 275 660 Tm (12) Tj 1 0 0 1 312 660 Tm (%) Tj \
             1 0 0 1 400 660 Tm (3) Tj ET",
        )
    }

    /// A timetable's lattice: a header band, then a rule every two rows.
    /// The two-line bands hold a minority of the claim's lines, so no rows
    /// are inferred in them; each line is still a record of figures.
    pub(crate) fn ruled_banded_records_content() -> String {
        String::from(
            "70 600 260 112 re S 150 600 m 150 712 l S 250 600 m 250 712 l S \
             70 700 m 330 700 l S 70 660 m 330 660 l S 70 620 m 330 620 l S \
             BT /F1 10 Tf 1 0 0 1 80 703 Tm (South) Tj 1 0 0 1 160 703 Tm (Times) Tj \
             1 0 0 1 260 703 Tm (Bronx) Tj \
             1 0 0 1 80 685 Tm (12:00) Tj 1 0 0 1 160 685 Tm (12:04) Tj 1 0 0 1 260 685 Tm (12:17) Tj \
             1 0 0 1 80 665 Tm (12:32) Tj 1 0 0 1 160 665 Tm (12:36) Tj 1 0 0 1 260 665 Tm (12:49) Tj \
             1 0 0 1 80 645 Tm (1:14) Tj 1 0 0 1 160 645 Tm (1:18) Tj 1 0 0 1 260 645 Tm (1:31) Tj \
             1 0 0 1 80 625 Tm (2:54) Tj 1 0 0 1 160 625 Tm (2:58) Tj 1 0 0 1 260 625 Tm (3:11) Tj \
             1 0 0 1 80 605 Tm (3:00) Tj 1 0 0 1 160 605 Tm (3:04) Tj 1 0 0 1 260 605 Tm (3:17) Tj ET",
        )
    }

    /// French amounts: the euro sign a word gap after its figure, and a
    /// rate with its percent sign a word gap after. The sign follows its
    /// amount here and must not jump onto the rate.
    pub(crate) fn euro_suffix_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (label, y, amount, rate) in [
            ("Nord", 700.0, "7 723", "51,4"),
            ("Sud", 680.0, "7 193", "51,7"),
            ("Est", 660.0, "6 734", "50,7"),
        ] {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({label}) Tj 1 0 0 1 275 {y} Tm ({amount}) Tj \
                 1 0 0 1 303 {y} Tm (\\200) Tj 1 0 0 1 330 {y} Tm ({rate}) Tj 1 0 0 1 352 {y} Tm (%) Tj "
            );
        }
        content += "ET";
        content
    }

    /// A statement's two ruled sections on the same verticals, the section
    /// label "Capital" unruled between them, the column heads above the
    /// top rule, and each row's label and amount both inside the first box.
    pub(crate) fn stacked_statement_content() -> String {
        let mut content = String::new();
        for (bottom, top) in [(650.0, 690.0), (590.0, 630.0)] {
            let mid = (bottom + top) / 2.0;
            for x in [70.0, 300.0, 430.0] {
                content += &format!("{x} {bottom} m {x} {top} l S ");
            }
            for y in [bottom, mid, top] {
                content += &format!("70 {y} m 430 {y} l S ");
            }
        }
        content += "BT /F1 10 Tf 1 0 0 1 75 700 Tm (Item) Tj 1 0 0 1 310 700 Tm (2024) Tj ";
        for (y, label, amount, value) in [
            (678.0, "Cash", "1,000", "5"),
            (658.0, "Debt", "2,000", "6"),
            (618.0, "Stock", "3,000", "7"),
            (598.0, "Total", "6,000", "18"),
        ] {
            content += &format!(
                "1 0 0 1 75 {y} Tm ({label}) Tj 1 0 0 1 250 {y} Tm ({amount}) Tj 1 0 0 1 310 {y} Tm ({value}) Tj "
            );
        }
        content += "1 0 0 1 75 637 Tm (Capital) Tj ET";
        content
    }

    /// Two boxed two-row grids on the verticals 70, 250 and 430 spanning
    /// the y ranges given, each row a label and an amount in the two cells,
    /// and `between` set as one line at y 638, under the upper box.
    fn two_boxes_content(boxes: [(f32, f32); 2], between: &str) -> String {
        let mut content = String::new();
        for (bottom, top) in boxes {
            let mid = (bottom + top) / 2.0;
            for x in [70.0, 250.0, 430.0] {
                content += &format!("{x} {bottom} m {x} {top} l S ");
            }
            for y in [bottom, mid, top] {
                content += &format!("70 {y} m 430 {y} l S ");
            }
            for (y, label, amount) in [
                (top - 12.0, "Deductible", "$500"),
                (bottom + 8.0, "LEM 03", "$39.00"),
            ] {
                content += &format!(
                    "BT /F1 10 Tf 1 0 0 1 75 {y} Tm ({label}) Tj 1 0 0 1 260 {y} Tm ({amount}) Tj ET "
                );
            }
        }
        if !between.is_empty() {
            content += &format!("BT /F1 10 Tf 1 0 0 1 75 638 Tm ({between}) Tj ET");
        }
        content
    }

    /// Two boxed grids with a line of prose between them that runs across
    /// the column rule: prose between two tables, not a label inside one.
    pub(crate) fn boxes_with_prose_between_content() -> String {
        two_boxes_content(
            [(650.0, 690.0), (590.0, 630.0)],
            "B. Premium if the endorsement is attached to the policy.",
        )
    }

    /// Two boxed grids with an empty gap of 220 points between them.
    pub(crate) fn boxes_far_apart_content() -> String {
        two_boxes_content([(650.0, 690.0), (390.0, 430.0)], "")
    }

    /// Two boxed three-row grids ten points apart on the same verticals,
    /// each opening with the column heads "Area" and "Factor": the
    /// sections of a rate table, each a table of its own.
    pub(crate) fn boxes_with_repeated_heads_content() -> String {
        let mut content = String::new();
        for (bottom, top, labels) in [
            (620.0, 680.0, ["Alpha", "Beta"]),
            (550.0, 610.0, ["Gamma", "Delta"]),
        ] {
            for x in [70.0, 250.0, 430.0] {
                content += &format!("{x} {bottom} m {x} {top} l S ");
            }
            for y in [bottom, bottom + 20.0, bottom + 40.0, top] {
                content += &format!("70 {y} m 430 {y} l S ");
            }
            let head = top - 12.0;
            content += &format!(
                "BT /F1 10 Tf 1 0 0 1 75 {head} Tm (Area) Tj 1 0 0 1 260 {head} Tm (Factor) Tj ET "
            );
            for (row, label) in labels.iter().enumerate() {
                let y = top - 32.0 - 20.0 * row as f32;
                content += &format!(
                    "BT /F1 10 Tf 1 0 0 1 75 {y} Tm ({label}) Tj 1 0 0 1 260 {y} Tm (0.70) Tj ET "
                );
            }
        }
        content
    }

    /// Two boxed grids ten points apart on the same verticals, each opening
    /// with the heads "Area" and "Trading Symbol", the lower box wrapping
    /// its second head onto two lines.
    pub(crate) fn boxes_with_wrapped_repeated_heads_content() -> String {
        let mut content = String::new();
        for (bottom, top) in [(620.0, 680.0), (550.0, 610.0)] {
            for x in [70.0, 250.0, 430.0] {
                content += &format!("{x} {bottom} m {x} {top} l S ");
            }
            for y in [bottom, bottom + 20.0, bottom + 40.0, top] {
                content += &format!("70 {y} m 430 {y} l S ");
            }
        }
        content +=
            "BT /F1 10 Tf 1 0 0 1 75 668 Tm (Area) Tj 1 0 0 1 260 668 Tm (Trading Symbol) Tj \
                    1 0 0 1 75 648 Tm (Alpha) Tj 1 0 0 1 260 648 Tm (0.70) Tj \
                    1 0 0 1 75 628 Tm (Beta) Tj 1 0 0 1 260 628 Tm (0.70) Tj \
                    1 0 0 1 75 604 Tm (Area) Tj 1 0 0 1 260 604 Tm (Trading ) Tj \
                    1 0 0 1 260 596 Tm (Symbol) Tj \
                    1 0 0 1 75 578 Tm (Gamma) Tj 1 0 0 1 260 578 Tm (0.70) Tj \
                    1 0 0 1 75 558 Tm (Delta) Tj 1 0 0 1 260 558 Tm (0.70) Tj ET";
        content
    }

    /// [`ruled_grid_content`] with a third band between the two rows that
    /// holds one whitespace span: the page's padding, not a row.
    pub(crate) fn ruled_blank_band_content() -> String {
        String::from(
            "70 650 360 60 re S 250 650 m 250 710 l S 70 690 m 430 690 l S 70 670 m 430 670 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 80 675 Tm ( ) Tj \
             1 0 0 1 80 655 Tm (a2) Tj 1 0 0 1 260 655 Tm (b2) Tj ET",
        )
    }

    /// Two lane tables of three columns, each with a rule under its header
    /// and a double rule under its total drawn under the amount columns
    /// alone: the rules of the two tables pair up into open lattices
    /// spanning both, whose lines run across the two drawn columns.
    pub(crate) fn two_tables_sharing_an_open_lattice_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for top in [700.0, 620.0] {
            for (offset, label, a, b) in [
                (0.0, "Year Ended", "2024", "2023"),
                (14.0, "Concentrate", "59", "58"),
                (28.0, "Finished", "41", "42"),
                (42.0, "Total", "100", "100"),
            ] {
                let y = top - offset;
                content += &format!(
                    "1 0 0 1 72 {y} Tm ({label}) Tj 1 0 0 1 300 {y} Tm ({a}) Tj 1 0 0 1 380 {y} Tm ({b}) Tj "
                );
            }
        }
        content += "ET ";
        for top in [700.0, 620.0] {
            for y in [top - 4.0, top - 46.0, top - 47.5] {
                content += &format!("290 {y} m 420 {y} l S ");
            }
        }
        content
    }

    /// A two-column list of fish ruled above its title and below its last
    /// row: an open lattice whose first row, the title, spans both
    /// columns, and whose sections between the rules are its rows.
    pub(crate) fn titled_open_lattice_content() -> String {
        let mut content = String::from(
            "55 665 m 290 665 l S 55 580 m 290 580 l S \
             BT /F1 10 Tf 1 0 0 1 60 651 Tm (Fish species on IUCN Red List) Tj ",
        );
        for (y, common, latin) in [
            (635.0, "Potosi Pupfish", "Cyprinodon alvarezi"),
            (619.0, "La Palma Pupfish", "Cyprinodon longidorsalis"),
            (603.0, "Butterfly Splitfin", "Ameca splendens"),
            (587.0, "Golden Skiffia", "Skiffia francesae"),
        ] {
            content += &format!("1 0 0 1 60 {y} Tm ({common}) Tj 1 0 0 1 160 {y} Tm ({latin}) Tj ");
        }
        content += "ET";
        content
    }

    /// A chart frame with four gridlines standing on a three-column table,
    /// the frame's verticals running down into the table's rules so the
    /// two weld into one lattice, and the chart's axis labels set left of
    /// the frame inside the lattice's bands.
    pub(crate) fn chart_over_table_content() -> String {
        String::from(
            "112 123 m 112 169 l S 190 123 m 190 411 l S 254 123 m 254 169 l S 317 123 m 317 411 l S \
             190 200 m 317 200 l S 190 250 m 317 250 l S 190 300 m 317 300 l S 190 350 m 317 350 l S \
             190 411 m 317 411 l S \
             112 123 m 317 123 l S 112 138 m 317 138 l S 112 153 m 317 153 l S 112 169 m 317 169 l S \
             BT /F1 8 Tf 1 0 0 1 160 305 Tm (90%) Tj 1 0 0 1 160 255 Tm (80%) Tj \
             1 0 0 1 160 205 Tm (70%) Tj \
             1 0 0 1 116 158 Tm (Rate) Tj 1 0 0 1 195 158 Tm (2024) Tj 1 0 0 1 259 158 Tm (2023) Tj \
             1 0 0 1 116 143 Tm (PIF) Tj 1 0 0 1 195 143 Tm (0.1%) Tj 1 0 0 1 259 143 Tm (6.9%) Tj \
             1 0 0 1 116 128 Tm (Total) Tj 1 0 0 1 195 128 Tm (1,461) Tj 1 0 0 1 259 128 Tm (95,491) Tj ET",
        )
    }

    /// Two three-column tables set to the same columns with a line of prose
    /// between them, each ruled under its header and its total across the
    /// full width: the rules chain into one open lattice, and the prose
    /// inside it reads as one cell over every column.
    pub(crate) fn two_tables_in_one_open_lattice_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for top in [700.0, 610.0] {
            for (offset, label, a, b) in [
                (0.0, "in millions", "2024", "2023"),
                (14.0, "Cash", "36,364", "48,677"),
                (28.0, "Loans", "46,694", "45,866"),
                (42.0, "Total", "83,058", "94,543"),
            ] {
                let y = top - offset;
                content += &format!(
                    "1 0 0 1 72 {y} Tm ({label}) Tj 1 0 0 1 220 {y} Tm ({a}) Tj 1 0 0 1 320 {y} Tm ({b}) Tj "
                );
            }
        }
        content += "1 0 0 1 72 632 Tm (The table below presents details about our loans.) Tj ET ";
        for top in [700.0, 610.0] {
            for y in [top - 4.0, top - 46.0, top - 47.5] {
                content += &format!("70 {y} m 380 {y} l S ");
            }
        }
        content
    }

    /// [`ruled_grid_content`] with the first column's text starting four
    /// points left of the left border rule.
    pub(crate) fn overhanging_ruled_content() -> String {
        String::from(
            "70 650 360 60 re S 250 650 m 250 710 l S 70 690 m 430 690 l S 70 670 m 430 670 l S \
             BT /F1 10 Tf 1 0 0 1 66 695 Tm (a1) Tj 1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 66 675 Tm (a2) Tj 1 0 0 1 260 675 Tm (b2) Tj \
             1 0 0 1 66 655 Tm (a3) Tj 1 0 0 1 260 655 Tm (b3) Tj ET",
        )
    }

    /// [`ruled_grid_content`] with a third row, the middle row's text one
    /// word set glyph by glyph running across the vertical rule at 250: the
    /// "m" starts four and a half points before the rule and ends past it.
    /// The top row's first cell runs to nine points short of the word, so
    /// no lane wide enough to split the drawn column stays clear.
    pub(crate) fn glyph_straddle_ruled_content() -> String {
        String::from(
            "70 650 360 60 re S 250 650 m 250 710 l S 70 690 m 430 690 l S 70 670 m 430 670 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (the first cell of the top row) Tj \
             1 0 0 1 260 695 Tm (b1) Tj \
             1 0 0 1 234 675 Tm (s) Tj 1 0 0 1 238 675 Tm (w) Tj \
             1 0 0 1 242 675 Tm (i) Tj 1 0 0 1 245.5 675 Tm (m) Tj \
             1 0 0 1 80 655 Tm (a3) Tj 1 0 0 1 260 655 Tm (b3) Tj ET",
        )
    }

    /// [`ruled_grid_content`] with a third row, each amount's currency sign
    /// floating four points left of the vertical rule its amount stands
    /// behind. The header's first cell runs to six points short of the
    /// signs, so no lane wide enough to split the drawn column stays clear.
    pub(crate) fn sign_before_rule_content() -> String {
        String::from(
            "70 650 360 60 re S 250 650 m 250 710 l S 70 690 m 430 690 l S 70 670 m 430 670 l S \
             BT /F1 10 Tf 1 0 0 1 80 695 Tm (Item description of the position) Tj \
             1 0 0 1 260 695 Tm (2024) Tj \
             1 0 0 1 80 675 Tm (Cash) Tj 1 0 0 1 246 675 Tm ($) Tj 1 0 0 1 252 675 Tm (1,000) Tj \
             1 0 0 1 80 655 Tm (Debt) Tj 1 0 0 1 246 655 Tm ($) Tj 1 0 0 1 252 655 Tm (2,000) Tj ET",
        )
    }

    /// [`lane_grid_content`] with a whitespace span standing in the first
    /// gutter of every row, a lane's width clear of the text on both sides:
    /// a producer's padding, which paints nothing.
    pub(crate) fn padded_gutter_grid_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
            content += &format!("1 0 0 1 160 {y} Tm (   ) Tj ");
        }
        content += "ET";
        content
    }

    /// A table of contents in two lanes: front matter paged in roman
    /// numerals, a part heading on a line of its own, then arabic pages.
    pub(crate) fn front_matter_contents_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (y, title, page) in [
            (700.0, "About the Publisher", "vii"),
            (686.0, "About This Project", "ix"),
            (672.0, "LAB MANUAL", ""),
            (658.0, "Experiment 1: Hydrostatic Pressure", "3"),
            (644.0, "Experiment 2: Bernoulli's Theorem", "13"),
            (630.0, "Experiment 3: Energy Loss in Pipes", "24"),
            (616.0, "References", "101"),
        ] {
            content += &format!("1 0 0 1 72 {y} Tm ({title}) Tj ");
            if !page.is_empty() {
                content += &format!("1 0 0 1 430 {y} Tm ({page}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// A four-lane table of volumes whose last column ends in "ml", the
    /// letters of a roman numeral: a contents list it is not.
    pub(crate) fn volumes_table_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (y, tube, water, glucose, yeast) in [
            (700.0, "Tube", "DI Water", "Glucose", "Yeast"),
            (686.0, "2", "24 ml", "0 ml", "4 ml"),
            (672.0, "3", "12 ml", "12 ml", "4 ml"),
            (658.0, "4", "4 ml", "12 ml", "12 ml"),
        ] {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({tube}) Tj 1 0 0 1 130 {y} Tm ({water}) Tj \
                 1 0 0 1 230 {y} Tm ({glucose}) Tj 1 0 0 1 330 {y} Tm ({yeast}) Tj "
            );
        }
        content += "ET";
        content
    }

    /// A table of contents set in three lanes: entry number, title, page
    /// number climbing down the list, two entries sharing a page.
    pub(crate) fn contents_list_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (number, y, title, page) in [
            ("1.", 700.0, "Front Matter", "1"),
            ("2.", 680.0, "Researching Wicked Problems", "3"),
            ("3.", 660.0, "Our Mental Shortcuts", "3"),
            ("4.", 640.0, "Identifying a Topic", "25"),
        ] {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({number}) Tj 1 0 0 1 100 {y} Tm ({title}) Tj 1 0 0 1 430 {y} Tm ({page}) Tj "
            );
        }
        content += "ET";
        content
    }

    /// A statement in lanes: a header, three rows, the section label
    /// "Paid-in Capital:" a blank line below them, and three more rows.
    pub(crate) fn sectioned_lane_table_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        content += "1 0 0 1 72 700 Tm (Item) Tj 1 0 0 1 250 700 Tm (2024) Tj 1 0 0 1 430 700 Tm (2023) Tj ";
        for (row, y) in [
            (0, 688.0),
            (1, 676.0),
            (2, 664.0),
            (3, 626.0),
            (4, 614.0),
            (5, 602.0),
        ] {
            if row == 3 {
                content += "1 0 0 1 72 638 Tm (Paid-in Capital:) Tj ";
            }
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// Three columns of 7-point type five points apart, four rows: the
    /// gaps are under the six-point gutter minimum and over two thirds of
    /// the type size.
    pub(crate) fn tight_lane_grid_content() -> String {
        let mut content = String::from("BT /F1 7 Tf ");
        for (row, y) in [(0, 700.0), (1, 690.0), (2, 680.0), (3, 670.0)] {
            for (col, x) in [(0, 72.0), (1, 92.0), (2, 112.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// A three-row lane grid, a prose line well below it, and a five-row
    /// grid below that: the longer grid is not the only one in the run.
    pub(crate) fn grid_above_a_longer_grid_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "1 0 0 1 72 615 Tm (Prose between the two grids.) Tj ";
        for (row, y) in [(0, 570.0), (1, 550.0), (2, 530.0), (3, 510.0), (4, 490.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (s{row}c{col}) Tj ");
            }
        }
        content += "ET";
        content
    }

    /// An open-ruled statement: rules over and under the header and under
    /// the first section's last row, then the label "Asset Class" and three
    /// unruled rows in the same columns, then a note and a line of prose.
    pub(crate) fn open_ruled_sections_content() -> String {
        String::from(
            "70 710 m 430 710 l S 70 688 m 430 688 l S 70 648 m 430 648 l S \
             BT /F1 10 Tf 1 0 0 1 72 700 Tm (Name) Tj 1 0 0 1 260 700 Tm (Kind) Tj \
             1 0 0 1 72 676 Tm (Pupfish) Tj 1 0 0 1 260 676 Tm (alvarezi) Tj \
             1 0 0 1 72 656 Tm (Skiffia) Tj 1 0 0 1 260 656 Tm (francesae) Tj \
             1 0 0 1 72 632 Tm (Asset Class) Tj \
             1 0 0 1 72 620 Tm (Goodeid) Tj 1 0 0 1 260 620 Tm (atripinnis) Tj \
             1 0 0 1 72 600 Tm (Splitfin) Tj 1 0 0 1 260 600 Tm (multiradiatus) Tj \
             1 0 0 1 72 580 Tm (Total) Tj 1 0 0 1 260 580 Tm (five) Tj \
             1 0 0 1 72 560 Tm (In the table above:) Tj \
             1 0 0 1 72 540 Tm (The species are listed by the year of their description.) Tj ET",
        )
    }

    /// A glossary in two lanes: six short terms and their definitions.
    pub(crate) fn glossary_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (y, term, meaning) in [
            (700.0, "Term", "Definition"),
            (686.0, "AED", "Advanced Electronic Data"),
            (672.0, "AFC", "Audit and Finance Committee of the Board"),
            (658.0, "APWU", "American Postal Workers Union"),
            (644.0, "ASC", "Accounting Standards Codification"),
            (630.0, "Board", "Board of Governors of the Postal Service"),
        ] {
            content += &format!("1 0 0 1 72 {y} Tm ({term}) Tj 1 0 0 1 160 {y} Tm ({meaning}) Tj ");
        }
        content += "ET";
        content
    }

    /// A three-column lane table whose amounts are set as one string each,
    /// the sign, three no-break spaces of padding and the digits.
    pub(crate) fn padded_sign_amount_lane_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (y, label, amount, rate) in [
            (700.0, "Item", "Amount", "Rate"),
            (686.0, "Cash", "$\\240\\240\\2401,414.00", "10%"),
            (672.0, "Debt", "$\\240\\240\\2402,120.50", "9%"),
            (658.0, "Fees", "$\\240\\240\\240310.00", "9%"),
        ] {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({label}) Tj 1 0 0 1 300 {y} Tm ({amount}) Tj 1 0 0 1 420 {y} Tm ({rate}) Tj "
            );
        }
        content += "ET";
        content
    }

    /// Five numbered items whose markers stand a lane's width from their
    /// text: a list, whatever the lane says.
    pub(crate) fn numbered_lane_list_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (index, y) in [700.0, 686.0, 672.0, 658.0, 644.0].into_iter().enumerate() {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({}.) Tj 1 0 0 1 92 {y} Tm (Item number {} of the list) Tj ",
                index + 1,
                index + 1
            );
        }
        content += "ET";
        content
    }

    /// A numbered item whose marker stands a lane from its text, then a
    /// two-column table set in from the margin, then a line of prose: the
    /// marker's lane stays clear through the table, so the item's one-lane
    /// stretch covers the table's, while the table's own lane lies
    /// elsewhere.
    pub(crate) fn list_then_two_column_table_content() -> String {
        let mut content = String::from(
            "BT /F1 10 Tf 1 0 0 1 54 700 Tm (1.) Tj 1 0 0 1 72 700 Tm (Adopt the following reference filings:) Tj \
             1 0 0 1 90 680 Tm (Description) Tj 1 0 0 1 262 680 Tm (Number) Tj ",
        );
        for (index, code) in [
            "GL-2013-BGL1",
            "CF-2013-RLA1",
            "CF-2012-RLA1",
            "CF-2011-RLA1",
            "CF-2010-RLA1",
            "CR-2007-RLA1",
        ]
        .iter()
        .enumerate()
        {
            let y = 666.0 - 14.0 * index as f32;
            content +=
                &format!("1 0 0 1 90 {y} Tm (Loss Costs) Tj 1 0 0 1 230 {y} Tm ({code}) Tj ");
        }
        content +=
            "1 0 0 1 54 560 Tm (Note that we have not written a full year of premium yet.) Tj ET";
        content
    }

    /// Two three-column lane tables one under the other at one pitch, each
    /// headed by the years 2018 and 2017 over a different first head.
    pub(crate) fn two_tables_repeating_their_head_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (y, label, a, b) in [
            (700.0, "Item", "2018", "2017"),
            (686.0, "Revenue", "79,591", "79,139"),
            (672.0, "Net income", "8,728", "5,753"),
            (658.0, "Basic", "9.56", "6.17"),
            (644.0, "At year end", "2018", "2017"),
            (630.0, "Total assets", "123,382", "125,356"),
            (616.0, "Total debt", "45,812", "46,824"),
            (602.0, "Total equity", "16,929", "17,725"),
        ] {
            content += &format!(
                "1 0 0 1 72 {y} Tm ({label}) Tj 1 0 0 1 300 {y} Tm ({a}) Tj 1 0 0 1 380 {y} Tm ({b}) Tj "
            );
        }
        content += "ET";
        content
    }

    /// Five lines of prose set in two columns too short for the gutter
    /// pass: each side takes near half the width.
    pub(crate) fn two_prose_columns_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for y in [700.0, 686.0, 672.0, 658.0, 644.0] {
            content += &format!(
                "1 0 0 1 72 {y} Tm (The left column runs its text to) Tj \
                 1 0 0 1 300 {y} Tm (and the right column does the same) Tj "
            );
        }
        content += "ET";
        content
    }

    /// Five references marked with numbers in parentheses, a lane from
    /// their text: a bibliography, not a two-column table.
    pub(crate) fn bracketed_reference_list_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (index, y) in [700.0, 686.0, 672.0, 658.0, 644.0].into_iter().enumerate() {
            content += &format!(
                "1 0 0 1 72 {y} Tm (({})) Tj 1 0 0 1 100 {y} Tm (Handbook of Chemistry, edition {}) Tj ",
                index + 10,
                index + 10
            );
        }
        content += "ET";
        content
    }

    /// Two lane grids of three rows each, one well below the other, with
    /// no prose between them: one segment, two tables.
    pub(crate) fn two_grids_content() -> String {
        let mut content = String::from("BT /F1 10 Tf ");
        for (row, y) in [(0, 700.0), (1, 680.0), (2, 660.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        for (row, y) in [(0, 500.0), (1, 480.0), (2, 460.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (s{row}c{col}) Tj ");
            }
        }
        content += "ET";
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
    // Covers ISO 32000-1 §14.8.2.5.
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
        assert_eq!(
            text,
            "Annual Report\nPrepared in June\nContact us at the office"
        );
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
    // Covers ISO 32000-1 §14.8.2.5.
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
    /// A fill-in table's blank body cells under a spanning header are its
    /// structure: no sign moved, so no column goes.
    #[test]
    fn blank_cells_keep_their_column() {
        let text = |t: &str| Cell {
            line: Some(Line {
                inlines: vec![Inline {
                    text: t.to_string(),
                    bold: false,
                    italic: false,
                    code: false,
                }],
                y: 0.0,
                x: 0.0,
                end_x: 10.0,
                size: 10.0,
            }),
            colspan: 1,
            rowspan: 1,
        };
        let header = vec![
            empty_cell(),
            Cell {
                colspan: 2,
                ..text("Mitosis Meiosis")
            },
        ];
        let body = vec![text("purpose"), text(" "), text(" ")];
        let mut rows = vec![header, body];
        tidy_amounts(&mut rows);
        assert_eq!(rows[0].iter().map(|c| c.colspan as usize).sum::<usize>(), 3);
        assert_eq!(rows[1].len(), 3);
        assert!(rows[1][1].line.is_some(), "a blank cell stays a cell");
    }

    /// Rules drawn every five points in a table set in 4-point type are one
    /// line per row; at body size the snap stays six points. The type that
    /// counts is the type between the close rules: a page of large body
    /// text around the small table does not fold its rows.
    #[test]
    fn tight_rules_stay_separate_lines_when_the_type_is_small() {
        let mut rulings = vec![
            ruling(70.0, 600.0, 70.0, 650.0),
            ruling(130.0, 600.0, 130.0, 650.0),
        ];
        for step in 0..=10 {
            let y = 600.0 + 5.0 * step as f32;
            rulings.push(ruling(70.0, y, 130.0, y));
        }
        let small: Vec<TextSpan> = (0..10)
            .map(|step| span("12", 72.0, 78.0, 601.0 + 5.0 * step as f32, 4.0))
            .collect();
        let snap = ruling_snap(&small, &rulings);
        assert!(snap < 5.0, "snap {snap}");
        let grids = ruled_grids(&rulings, snap);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].ys.len(), 11, "ys {:?}", grids[0].ys);
        let body = [span("body", 72.0, 100.0, 700.0, 10.0)];
        assert_eq!(ruling_snap(&body, &rulings), RULING_SNAP_TOLERANCE);
        assert_eq!(ruling_snap(&small, &[]), RULING_SNAP_TOLERANCE);
        let mut page = small.clone();
        page.extend((0..20).map(|line| {
            span(
                "a paragraph of body text far below",
                72.0,
                300.0,
                400.0 - 12.0 * line as f32,
                10.0,
            )
        }));
        let snap = ruling_snap(&page, &rulings);
        assert!(
            snap < 5.0,
            "the body text outside the rules has no say: {snap}"
        );
    }

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
        let grids = ruled_grids(
            &boxed_grid_rulings(70.0, 630.0, 430.0, 710.0),
            RULING_SNAP_TOLERANCE,
        );
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
        let grids = ruled_grids(&rulings, RULING_SNAP_TOLERANCE);
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
        let grids = ruled_grids(&rulings, RULING_SNAP_TOLERANCE);
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
        assert!(ruled_grids(&rulings, RULING_SNAP_TOLERANCE).is_empty());
        assert!(
            ruled_grids(&[ruling(70.0, 400.0, 430.0, 400.0)], RULING_SNAP_TOLERANCE).is_empty()
        );
    }

    /// A ruling that crosses nothing in the lattice — an underline elsewhere
    /// on the page — must not join it or open a phantom column.
    #[test]
    fn an_unconnected_ruling_stays_out_of_the_lattice() {
        let mut rulings = boxed_grid_rulings(70.0, 600.0, 430.0, 680.0);
        rulings.push(ruling(70.0, 100.0, 200.0, 100.0));
        let grids = ruled_grids(&rulings, RULING_SNAP_TOLERANCE);
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

    /// A label is read in the list's numbering system and only in it:
    /// digits for Decimal, letters of the system's case (repeated past Z)
    /// for the alphabetic systems, numerals of the system's case for the
    /// Roman ones, each closed by `.` or `)` or by nothing.
    // Covers ISO 32000-1 §14.8.5.5.
    #[test]
    fn labels_are_read_in_the_list_s_numbering_system() {
        assert_eq!(numbered_label("3.", ListNumbering::Decimal), Some(3));
        assert_eq!(numbered_label("A.", ListNumbering::Decimal), None);
        assert_eq!(numbered_label("B)", ListNumbering::UpperAlpha), Some(2));
        assert_eq!(numbered_label("AA", ListNumbering::UpperAlpha), Some(27));
        assert_eq!(numbered_label("b.", ListNumbering::UpperAlpha), None);
        assert_eq!(numbered_label("c.", ListNumbering::LowerAlpha), Some(3));
        assert_eq!(numbered_label("iv.", ListNumbering::LowerRoman), Some(4));
        assert_eq!(numbered_label("xiv", ListNumbering::LowerRoman), Some(14));
        assert_eq!(numbered_label("IV.", ListNumbering::LowerRoman), None);
        assert_eq!(
            numbered_label("MCMXC.", ListNumbering::UpperRoman),
            Some(1990)
        );
        assert_eq!(numbered_label("", ListNumbering::Decimal), None);
        assert_eq!(numbered_label("-", ListNumbering::Decimal), None);
    }
}
