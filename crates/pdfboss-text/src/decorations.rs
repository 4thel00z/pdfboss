//! Span decorations read from the page: underline, strikethrough and
//! highlight from the rulings and filled bands the content draws, and from
//! the text markup annotations the page carries.

use crate::extract::components_color;
use crate::{Ruling, TextSpan};
use pdfboss_core::{AsyncObjectSource, Dict, Object, OcState, Page, Rect};

/// A filled rectangle too thick to be a ruling: a highlight candidate.
pub(crate) struct Band {
    pub rect: Rect,
    /// The fill color as RGB in `[0, 1]`.
    pub color: (f32, f32, f32),
    /// How many spans the walk had emitted when the band was painted. A
    /// highlight lies behind its text, so only later spans can be its.
    pub spans_before: usize,
}

/// How far below the baseline (in fractions of the effective size) an
/// underline may sit, how far below the box bottom when the font's descent
/// reaches deeper than that, and the slack above the baseline for lines
/// drawn exactly on it.
const UNDERLINE_BELOW: f32 = 0.3;
const UNDERLINE_BELOW_BOX: f32 = 0.1;
const UNDERLINE_ABOVE: f32 = 0.05;

/// The x-height band (in fractions of the effective size above the
/// baseline) a strikethrough crosses.
const STRIKETHROUGH_LOW: f32 = 0.15;
const STRIKETHROUGH_HIGH: f32 = 0.6;

/// The fraction of a span's width a decoration must cover to mark it: a
/// neighbour's underline running past the end of its word is not this span's.
const DECORATED_MIN_OVERLAP: f32 = 0.6;

/// How far, in ems of the covered text, a drawn decoration may run past
/// the text it covers on either side, and the widest gap between covered
/// spans that still reads as one run of text. A cell border runs to the
/// cell edge and paragraph shading to the margin; an underline or a
/// highlight stops where the text does.
const OVERHANG_EM: f32 = 1.0;

/// The fraction of a span's box height a highlight must cover.
const HIGHLIGHT_MIN_COVER: f32 = 0.5;

/// The tallest a highlight band may be, in span box heights: one line with
/// room for leading. A panel or a table background is taller.
const HIGHLIGHT_MAX_HEIGHT: f32 = 2.0;

/// The least spread between a highlight's strongest and weakest RGB
/// component: a marker has a hue. White and gray bands are backgrounds,
/// knockouts and cell fills.
const HIGHLIGHT_MIN_CHROMA: f32 = 0.1;

/// How far apart, in device units, two bands' edges may be and still tile
/// one shaded area.
const TILING_EPSILON: f32 = 0.5;

/// Sets `underline`, `strikethrough` and `highlight` from what the page
/// drew: horizontal rulings and filled bands. Vertical writing is left
/// unmarked; its decorations are vertical lines beside the text, which
/// are indistinguishable from column rules here.
pub(crate) fn mark_drawn(spans: &mut [TextSpan], rulings: &[Ruling], bands: &[Band]) {
    let horizontals: Vec<&Ruling> = rulings.iter().filter(|r| r.start.y == r.end.y).collect();
    if horizontals.is_empty() && bands.is_empty() {
        return;
    }
    let lines = Lines::new(spans);
    let mut covered = Vec::new();
    for ruling in horizontals {
        let y = ruling.start.y;
        covered.clear();
        for &i in
            lines.baselines_between(y - STRIKETHROUGH_HIGH * lines.max_size, y + lines.max_drop)
        {
            let span = &spans[i];
            if !decorable(span) || ruling_kind(span, y).is_none() {
                continue;
            }
            if overlap(ruling.start.x, ruling.end.x, span.bbox.x0, span.bbox.x1) <= 0.0 {
                continue;
            }
            covered.push(i);
        }
        let Some(run) = fitting_run(spans, &mut covered, ruling.start.x, ruling.end.x) else {
            continue;
        };
        mark_covered(
            spans,
            &covered[run],
            ruling.start.x,
            ruling.end.x,
            |span| match ruling_kind(span, y) {
                Some(Drawn::Underline) => span.underline = true,
                Some(Drawn::Strikethrough) => span.strikethrough = true,
                None => {}
            },
        );
    }
    for region in regions(bands) {
        let mut runs = Vec::with_capacity(region.len());
        for band in &region {
            let rect = band.rect;
            let mut covered = Vec::new();
            let range =
                lines.baselines_between(rect.y0 - lines.max_ascent, rect.y1 - lines.min_descent);
            for &i in range {
                if i < band.spans_before || !highlightable(&spans[i], band) {
                    continue;
                }
                covered.push(i);
            }
            let Some(run) = fitting_run(spans, &mut covered, rect.x0, rect.x1) else {
                runs.clear();
                break;
            };
            runs.push((covered, run));
        }
        for (band, (covered, run)) in region.iter().zip(runs) {
            mark_covered(spans, &covered[run], band.rect.x0, band.rect.x1, |span| {
                if span.highlight {
                    return;
                }
                span.highlight = true;
                span.highlight_color = Some(band.color);
            });
        }
    }
}

/// Groups the bands that tile one shaded area: vertically adjacent, of one
/// color and one horizontal extent. Paragraph shading and cell backgrounds
/// come as such stacks, one band per line, every band running to the box
/// edge; a marker over a passage comes as one band per line too, but each
/// ends where its line's text does. A region is a highlight only when every
/// band in it fits its own line's text, so one shaded line whose text
/// happens to fill the row does not read as marked.
fn regions(bands: &[Band]) -> Vec<Vec<&Band>> {
    let mut order: Vec<&Band> = bands.iter().collect();
    order.sort_by(|a, b| {
        let key = |band: &Band| {
            let (r, g, bl) = band.color;
            [r, g, bl, band.rect.x0, band.rect.x1, band.rect.y0]
        };
        key(a)
            .iter()
            .zip(key(b))
            .map(|(x, y)| x.total_cmp(&y))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut regions: Vec<Vec<&Band>> = Vec::new();
    for band in order {
        let extends = regions.last().is_some_and(|region| {
            let last = region[region.len() - 1];
            last.color == band.color
                && (last.rect.x0 - band.rect.x0).abs() <= TILING_EPSILON
                && (last.rect.x1 - band.rect.x1).abs() <= TILING_EPSILON
                && band.rect.y0 <= last.rect.y1 + TILING_EPSILON
        });
        if extends {
            regions.last_mut().unwrap().push(band);
            continue;
        }
        regions.push(vec![band]);
    }
    regions
}

/// Which mark a horizontal ruling at `y` makes on `span`, by where it
/// crosses the span's box: an underline just below the baseline, a
/// strikethrough through the x-height band, or nothing.
fn ruling_kind(span: &TextSpan, y: f32) -> Option<Drawn> {
    let above = y - span.y;
    let size = span.size;
    if above >= underline_floor(span) && above <= UNDERLINE_ABOVE * size {
        return Some(Drawn::Underline);
    }
    if above >= STRIKETHROUGH_LOW * size && above <= STRIKETHROUGH_HIGH * size {
        return Some(Drawn::Strikethrough);
    }
    None
}

enum Drawn {
    Underline,
    Strikethrough,
}

/// The lowest a ruling may sit below the baseline and still underline the
/// span: [`UNDERLINE_BELOW`] of the size, or [`UNDERLINE_BELOW_BOX`] under
/// the box bottom for a font whose descent reaches deeper. A descriptor
/// with a shallow or missing `/Descent` cannot narrow the window.
fn underline_floor(span: &TextSpan) -> f32 {
    (-UNDERLINE_BELOW * span.size).min(span.descent - UNDERLINE_BELOW_BOX * span.size)
}

/// Whether `band` can be `span`'s highlight: about a line tall, covering
/// the span's box, colored, and lighter than the text painted over it.
fn highlightable(span: &TextSpan, band: &Band) -> bool {
    if !decorable(span) || chroma(band.color) < HIGHLIGHT_MIN_CHROMA {
        return false;
    }
    let height = span.bbox.height();
    if height <= 0.0 || band.rect.height() > HIGHLIGHT_MAX_HEIGHT * height {
        return false;
    }
    if overlap(band.rect.y0, band.rect.y1, span.bbox.y0, span.bbox.y1)
        < HIGHLIGHT_MIN_COVER * height
    {
        return false;
    }
    if overlap(band.rect.x0, band.rect.x1, span.bbox.x0, span.bbox.x1) <= 0.0 {
        return false;
    }
    let Some(text) = span.color else {
        return false;
    };
    gray(band.color) > gray(text)
}

/// Whether drawn decorations apply to the span at all.
fn decorable(span: &TextSpan) -> bool {
    !span.vertical && span.size > 0.0 && span.bbox.width() > 0.0
}

/// The spread between an RGB color's strongest and weakest component:
/// zero for white, black and every gray.
fn chroma((r, g, b): (f32, f32, f32)) -> f32 {
    r.max(g).max(b) - r.min(g).min(b)
}

/// The gray level of an RGB color, by the conversion ISO 32000-1 §10.3.2
/// gives for device color spaces.
fn gray((r, g, b): (f32, f32, f32)) -> f32 {
    0.3 * r + 0.59 * g + 0.11 * b
}

/// The length of the intersection of `[a0, a1]` and `[b0, b1]`, negative
/// when they are apart.
fn overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    a1.min(b1) - a0.max(b0)
}

/// The run of text a decoration spanning `x0..x1` fits, as a range into
/// `covered`, which holds every span the decoration overlaps at all and is
/// sorted by x here. The covered spans split into runs wherever the gap
/// between neighbours exceeds an em; the decoration fits the run it stops
/// within an em of on both ends. `None` when it fits no run: a border under
/// several cells, shading past the text.
fn fitting_run(
    spans: &[TextSpan],
    covered: &mut [usize],
    x0: f32,
    x1: f32,
) -> Option<std::ops::Range<usize>> {
    covered.sort_by(|&a, &b| spans[a].bbox.x0.total_cmp(&spans[b].bbox.x0));
    let mut start = 0;
    while start < covered.len() {
        let mut end = start + 1;
        let mut run_x1 = spans[covered[start]].bbox.x1;
        let mut em = spans[covered[start]].size;
        while end < covered.len() {
            let next = &spans[covered[end]];
            if next.bbox.x0 - run_x1 > OVERHANG_EM * em.max(next.size) {
                break;
            }
            run_x1 = run_x1.max(next.bbox.x1);
            em = em.max(next.size);
            end += 1;
        }
        let run_x0 = spans[covered[start]].bbox.x0;
        if x0 >= run_x0 - OVERHANG_EM * em && x1 <= run_x1 + OVERHANG_EM * em {
            return Some(start..end);
        }
        start = end;
    }
    None
}

/// Applies `mark` to each span of `run` the decoration spanning `x0..x1`
/// covers by [`DECORATED_MIN_OVERLAP`].
fn mark_covered(
    spans: &mut [TextSpan],
    run: &[usize],
    x0: f32,
    x1: f32,
    mut mark: impl FnMut(&mut TextSpan),
) {
    for &i in run {
        let span = &mut spans[i];
        if overlap(x0, x1, span.bbox.x0, span.bbox.x1) < DECORATED_MIN_OVERLAP * span.bbox.width() {
            continue;
        }
        mark(span);
    }
}

/// The page's spans indexed by baseline, with the bounds a decoration's
/// candidate range is widened by, so each ruling or band consults only the
/// spans in its vertical neighbourhood.
struct Lines {
    /// Span indices sorted by baseline.
    order: Vec<usize>,
    /// The baselines, in `order`.
    baselines: Vec<f32>,
    max_size: f32,
    /// The furthest any span's underline window reaches below its
    /// baseline.
    max_drop: f32,
    max_ascent: f32,
    min_descent: f32,
}

impl Lines {
    fn new(spans: &[TextSpan]) -> Lines {
        let mut order: Vec<usize> = (0..spans.len()).collect();
        order.sort_by(|&a, &b| spans[a].y.total_cmp(&spans[b].y));
        let baselines = order.iter().map(|&i| spans[i].y).collect();
        let mut lines = Lines {
            order,
            baselines,
            max_size: 0.0,
            max_drop: 0.0,
            max_ascent: 0.0,
            min_descent: 0.0,
        };
        for span in spans {
            lines.max_size = lines.max_size.max(span.size);
            lines.max_drop = lines.max_drop.max(-underline_floor(span));
            lines.max_ascent = lines.max_ascent.max(span.ascent);
            lines.min_descent = lines.min_descent.min(span.descent);
        }
        lines
    }

    /// The indices of the spans whose baseline lies in `[low, high]`.
    fn baselines_between(&self, low: f32, high: f32) -> &[usize] {
        let first = self.baselines.partition_point(|&y| y < low);
        let last = self.baselines.partition_point(|&y| y <= high);
        &self.order[first..last.max(first)]
    }
}

/// One text markup annotation's coverage: the mark it makes, its color,
/// and the box of each quadrilateral it marks.
pub(crate) struct Markup {
    kind: MarkupKind,
    color: Option<(f32, f32, f32)>,
    boxes: Vec<Rect>,
}

/// The text markup annotation subtypes (ISO 32000-1 §12.5.6.10) and the
/// span flag each sets. A squiggly underline is an underline.
enum MarkupKind {
    Highlight,
    Underline,
    StrikeOut,
}

impl MarkupKind {
    fn of_subtype(name: &str) -> Option<MarkupKind> {
        match name {
            "Highlight" => Some(MarkupKind::Highlight),
            "Underline" | "Squiggly" => Some(MarkupKind::Underline),
            "StrikeOut" => Some(MarkupKind::StrikeOut),
            _ => None,
        }
    }
}

/// Annotation flags (ISO 32000-1 §12.5.3) under which an annotation is
/// not shown: Hidden (bit 2) and NoView (bit 6).
const UNSHOWN_ANNOTATION: i64 = (1 << 1) | (1 << 5);

/// The page's text markup annotations: `/Highlight`, `/Underline`,
/// `/Squiggly` and `/StrikeOut`, each with the box of every `/QuadPoints`
/// quadrilateral (taken by extent, whatever order the vertices come in)
/// or, without usable quadrilaterals, its `/Rect`. Annotations flagged
/// Hidden or NoView, or hidden by their `/OC` entry, mark nothing; an
/// unreadable annotation is skipped.
///
/// Covers ISO 32000-1 §12.5.6.10.
pub(crate) async fn markup_annotations<S: AsyncObjectSource>(
    src: &S,
    page: &Page,
    oc: Option<&OcState>,
) -> Vec<Markup> {
    let mut markups = Vec::new();
    let Some(annots) = page.dict().get("Annots") else {
        return markups;
    };
    let Ok(Object::Array(items)) = src.resolve(annots).await else {
        return markups;
    };
    for item in &items {
        let Ok(resolved) = src.resolve(item).await else {
            continue;
        };
        let Some(dict) = resolved.as_dict() else {
            continue;
        };
        let Some(kind) = dict
            .get_name("Subtype")
            .and_then(|n| MarkupKind::of_subtype(&n.0))
        else {
            continue;
        };
        if dict.get_int("F").unwrap_or(0) & UNSHOWN_ANNOTATION != 0 {
            continue;
        }
        if let (Some(oc), Some(gate)) = (oc, dict.get("OC")) {
            if !oc.visible_with(src, gate).await {
                continue;
            }
        }
        let boxes = match quad_boxes(src, dict).await {
            Some(boxes) => boxes,
            None => match rect_of(src, dict).await {
                Some(rect) => vec![rect],
                None => continue,
            },
        };
        let color = match numbers(src, dict, "C").await {
            Some(components) => components_color(&components),
            None => None,
        };
        markups.push(Markup { kind, color, boxes });
    }
    markups
}

/// The box of each quadrilateral in the annotation's `/QuadPoints`: eight
/// numbers per quadrilateral, an incomplete trailing group ignored. `None`
/// without the entry or with fewer than eight numbers.
async fn quad_boxes<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Option<Vec<Rect>> {
    let quads = numbers(src, annot, "QuadPoints").await?;
    let (groups, _) = quads.as_chunks::<8>();
    let boxes: Vec<Rect> = groups
        .iter()
        .map(|q| {
            let xs = [q[0], q[2], q[4], q[6]];
            let ys = [q[1], q[3], q[5], q[7]];
            Rect::new(
                xs.iter().copied().fold(f32::INFINITY, f32::min),
                ys.iter().copied().fold(f32::INFINITY, f32::min),
                xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            )
        })
        .collect();
    (!boxes.is_empty()).then_some(boxes)
}

/// The annotation's `/Rect`, normalized.
async fn rect_of<S: AsyncObjectSource>(src: &S, annot: &Dict) -> Option<Rect> {
    let r = numbers(src, annot, "Rect").await?;
    let [x0, y0, x1, y1] = r[..] else {
        return None;
    };
    Some(Rect::new(x0, y0, x1, y1).normalize())
}

/// The numbers in the array under `key`, every element resolved. `None`
/// without the entry, when it is not an array, or when any element is not
/// a finite number.
async fn numbers<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Vec<f32>> {
    let resolved = src.resolve(dict.get(key)?).await.ok()?;
    let items = resolved.as_array()?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let value = src.resolve(item).await.ok()?.as_f64()? as f32;
        if !value.is_finite() {
            return None;
        }
        out.push(value);
    }
    Some(out)
}

/// Sets the flags the page's text markup annotations declare: a span is
/// marked by every quadrilateral that covers most of its width and at
/// least half its box height. Authored markup, so no test of color, paint
/// order or extent applies.
pub(crate) fn mark_annotated(spans: &mut [TextSpan], markups: &[Markup]) {
    for markup in markups {
        for rect in &markup.boxes {
            for span in spans.iter_mut() {
                if !decorable(span) {
                    continue;
                }
                let height = span.bbox.height();
                if overlap(rect.y0, rect.y1, span.bbox.y0, span.bbox.y1)
                    < HIGHLIGHT_MIN_COVER * height
                {
                    continue;
                }
                let width = span.bbox.width();
                if overlap(rect.x0, rect.x1, span.bbox.x0, span.bbox.x1)
                    < DECORATED_MIN_OVERLAP * width
                {
                    continue;
                }
                match markup.kind {
                    MarkupKind::Underline => span.underline = true,
                    MarkupKind::StrikeOut => span.strikethrough = true,
                    MarkupKind::Highlight => {
                        if span.highlight {
                            continue;
                        }
                        span.highlight = true;
                        span.highlight_color = markup.color;
                    }
                }
            }
        }
    }
}
