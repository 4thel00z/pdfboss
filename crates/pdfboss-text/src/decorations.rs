//! Drawn-decoration and text-markup classification for [`TextSpan`] flags.
//!
//! PDF has no underline / strikethrough / highlight attributes. These
//! flags are read from page geometry (thin rulings and filled bands) and
//! from text-markup annotations (`/Underline`, `/StrikeOut`, `/Highlight`).
//! `/Link` is ignored.

use crate::Ruling;
use crate::TextSpan;
use pdfboss_core::{AsyncObjectSource, Object, Page, Point, Rect};

/// A filled axis-aligned rectangle kept as highlight evidence: bbox in the
/// same y-up user space as [`TextSpan`], plus the resolved DeviceRGB fill.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FillBand {
    pub bbox: Rect,
    pub color: (f32, f32, f32),
}

/// A text-markup annotation reduced to a kind and one rect per marked line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Markup {
    pub kind: MarkupKind,
    pub rects: Vec<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkupKind {
    Underline,
    StrikeOut,
    Highlight,
}

/// Fraction of a span's width a ruling must cover to decorate it.
const DECORATED_MIN_OVERLAP: f32 = 0.6;

/// Strikethrough band as a fraction of the glyph box, measured from the
/// bottom: a bar crossing the body of the glyphs.
const STRIKE_LOW: f32 = 0.38;
const STRIKE_HIGH: f32 = 0.62;

/// Underline band as a fraction of the glyph box from the bottom: just
/// below the baseline / near the descent. 0.72–1.04 from the top of a
/// y-down tight box is −0.04–0.28 from the bottom of a y-up box.
const UNDERLINE_LOW: f32 = -0.04;
const UNDERLINE_HIGH: f32 = 0.28;

/// Area coverage at which an annotation rect inherits onto a span.
const ANNOT_OVERLAP: f32 = 0.2;

/// Highlight matching: strong horizontal overlap, some vertical overlap,
/// and the band's midline inside a vertically expanded span box.
const HIGHLIGHT_H_OVERLAP: f32 = 0.52;
const HIGHLIGHT_V_OVERLAP: f32 = 0.08;

/// Highlight band height in PDF points (line-height, not a hairline).
const HIGHLIGHT_MIN_HEIGHT: f32 = 1.44;
const HIGHLIGHT_MAX_HEIGHT: f32 = 14.4;

/// A highlight must not be a full-row wash. The same cap drops table
/// borders and header rules from underline / strikethrough matching.
const HIGHLIGHT_MAX_PAGE_FRACTION: f32 = 0.55;

/// Text darker than this luminance can sit on a highlight bar.
const DARK_TEXT_LUMA: f32 = 0.55;

/// Sets `underline` / `strikethrough` from horizontal rulings. A bar that
/// crosses the glyph body is strikethrough; a bar just below the baseline
/// is underline. The two windows do not overlap, so a mid-glyph thin fill
/// cannot be reported as an underline. Page-wide rules (table borders,
/// header separators) are ignored — they cover every span on the row.
pub(crate) fn mark_underline_and_strikethrough(
    span: &mut TextSpan,
    horizontals: &[&Ruling],
    page_width: f32,
) {
    if span.vertical || span.size <= 0.0 {
        return;
    }
    let width = span.bbox.x1 - span.bbox.x0;
    let height = span.bbox.y1 - span.bbox.y0;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let max_rule = page_width.max(1.0) * HIGHLIGHT_MAX_PAGE_FRACTION;
    let low = span.bbox.y0 + UNDERLINE_LOW * height;
    let high = span.bbox.y0 + STRIKE_HIGH * height;
    let first = horizontals.partition_point(|r| r.start.y < low);
    for r in &horizontals[first..] {
        if r.start.y > high {
            break;
        }
        let rule_w = r.end.x - r.start.x;
        if rule_w > max_rule {
            continue;
        }
        let overlap = r.end.x.min(span.bbox.x1) - r.start.x.max(span.bbox.x0);
        if overlap < DECORATED_MIN_OVERLAP * width {
            continue;
        }
        let rel = (r.start.y - span.bbox.y0) / height;
        if (STRIKE_LOW..=STRIKE_HIGH).contains(&rel) {
            span.strikethrough = true;
        } else if (UNDERLINE_LOW..=UNDERLINE_HIGH).contains(&rel) {
            span.underline = true;
        }
    }
}

/// Sets `highlight` (and `highlight_color`) from filled line-height bands
/// whose color looks like a marker and that sit behind dark text.
pub(crate) fn mark_highlights(span: &mut TextSpan, fills: &[FillBand]) {
    if span.vertical || !is_dark_text(span.color) {
        return;
    }
    for band in fills {
        if !band_matches_span(span, band.bbox, true) {
            continue;
        }
        span.highlight = true;
        if span.highlight_color.is_none() {
            span.highlight_color = Some(band.color);
        }
        return;
    }
}

/// ORs annotation-backed styling into the same three flags.
pub(crate) fn mark_markup(span: &mut TextSpan, markups: &[Markup]) {
    if span.vertical {
        return;
    }
    for markup in markups {
        let highlight = matches!(markup.kind, MarkupKind::Highlight);
        if !markup
            .rects
            .iter()
            .any(|rect| band_matches_span(span, *rect, highlight))
        {
            continue;
        }
        match markup.kind {
            MarkupKind::Underline => span.underline = true,
            MarkupKind::StrikeOut => span.strikethrough = true,
            MarkupKind::Highlight => span.highlight = true,
        }
    }
}

fn band_matches_span(span: &TextSpan, band: Rect, highlight: bool) -> bool {
    if rect_overlap_fraction(span.bbox, band) >= ANNOT_OVERLAP {
        return true;
    }
    if !highlight {
        return false;
    }
    let span_h = span.bbox.y1 - span.bbox.y0;
    let span_w = span.bbox.x1 - span.bbox.x0;
    if span_h <= 0.0 || span_w <= 0.0 {
        return false;
    }
    let horizontal = axis_overlap(span.bbox.x0, span.bbox.x1, band.x0, band.x1) / span_w;
    let vertical = axis_overlap(span.bbox.y0, span.bbox.y1, band.y0, band.y1) / span_h;
    let mid = (band.y0 + band.y1) / 2.0;
    let expanded_lo = span.bbox.y0 - span_h * 0.25;
    let expanded_hi = span.bbox.y1 + span_h * 0.20;
    horizontal >= HIGHLIGHT_H_OVERLAP
        && vertical >= HIGHLIGHT_V_OVERLAP
        && mid >= expanded_lo
        && mid <= expanded_hi
}

fn rect_overlap_fraction(a: Rect, b: Rect) -> f32 {
    let area = (a.x1 - a.x0).max(0.0) * (a.y1 - a.y0).max(0.0);
    if area <= 0.0 {
        return 0.0;
    }
    let ix = axis_overlap(a.x0, a.x1, b.x0, b.x1);
    let iy = axis_overlap(a.y0, a.y1, b.y0, b.y1);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    (ix * iy) / area
}

fn axis_overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    (a1.min(b1) - a0.max(b0)).max(0.0)
}

fn is_dark_text(color: Option<(f32, f32, f32)>) -> bool {
    let (r, g, b) = color.unwrap_or((0.0, 0.0, 0.0));
    0.2126 * r + 0.7152 * g + 0.0722 * b < DARK_TEXT_LUMA
}

/// Whether an RGB fill (0–1) looks like a text highlight: saturated marker
/// colours or a pale pink, not black rules or white/gray washes.
pub(crate) fn is_highlight_fill(r: f32, g: f32, b: f32) -> bool {
    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    if max_c < 0.45 || min_c > 0.92 {
        return false;
    }
    if max_c - min_c >= 0.18 {
        return true;
    }
    max_c >= 0.82 && min_c <= 0.88 && (max_c - min_c) >= 0.08
}

/// Size-gate a filled rect into a highlight band: line-height, not a
/// full-row wash or a hairline.
pub(crate) fn is_highlight_size(bbox: Rect, page_width: f32) -> bool {
    let w = bbox.x1 - bbox.x0;
    let h = bbox.y1 - bbox.y0;
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    if w > page_width.max(1.0) * HIGHLIGHT_MAX_PAGE_FRACTION {
        return false;
    }
    (HIGHLIGHT_MIN_HEIGHT..=HIGHLIGHT_MAX_HEIGHT).contains(&h)
}

/// Axis-aligned bounding box of a closed rectangular subpath in device
/// space. `None` when the path is not a 4-corner (optionally closed)
/// axis-aligned rectangle.
pub(crate) fn filled_rect_bbox(device: &[Point]) -> Option<Rect> {
    let corners = match device {
        [a, b, c, d] => [*a, *b, *c, *d],
        [a, b, c, d, e] if (e.x - a.x).abs() <= 0.5 && (e.y - a.y).abs() <= 0.5 => [*a, *b, *c, *d],
        _ => return None,
    };
    if corners.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return None;
    }
    let axis_aligned = |a: Point, b: Point| (b.x - a.x).abs() <= 0.5 || (b.y - a.y).abs() <= 0.5;
    for i in 0..4 {
        if !axis_aligned(corners[i], corners[(i + 1) % 4]) {
            return None;
        }
    }
    let x0 = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let x1 = corners
        .iter()
        .map(|p| p.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let y0 = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let y1 = corners
        .iter()
        .map(|p| p.y)
        .fold(f32::NEG_INFINITY, f32::max);
    if !x0.is_finite() {
        return None;
    }
    Some(Rect { x0, y0, x1, y1 })
}

/// Reads `/Annots` and keeps Underline / StrikeOut / Highlight. Link and
/// unknown subtypes are skipped; one unreadable annot never fails the page.
///
/// Covers ISO 32000-1 §12.5.6.10.
pub(crate) async fn markup_annotations<S: AsyncObjectSource>(src: &S, page: &Page) -> Vec<Markup> {
    let Some(annots) = page.dict().get("Annots") else {
        return Vec::new();
    };
    let Ok(resolved) = src.resolve(annots).await else {
        return Vec::new();
    };
    let Some(items) = resolved.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let Ok(obj) = src.resolve(item).await else {
            continue;
        };
        let Some(dict) = obj.as_dict() else {
            continue;
        };
        let Some(subtype) = dict.get_name("Subtype").map(|n| n.0.as_str()) else {
            continue;
        };
        let kind = match subtype {
            "Underline" => MarkupKind::Underline,
            "StrikeOut" => MarkupKind::StrikeOut,
            "Highlight" => MarkupKind::Highlight,
            _ => continue,
        };
        let mut rects = read_quads(src, dict.get("QuadPoints")).await;
        if rects.is_empty() {
            if let Some(rect) = read_rect(src, dict.get("Rect")).await {
                rects.push(rect);
            }
        }
        if !rects.is_empty() {
            out.push(Markup { kind, rects });
        }
    }
    out
}

async fn read_rect<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Option<Rect> {
    let nums = read_numbers(src, obj).await;
    (nums.len() >= 4).then(|| Rect {
        x0: nums[0].min(nums[2]),
        y0: nums[1].min(nums[3]),
        x1: nums[0].max(nums[2]),
        y1: nums[1].max(nums[3]),
    })
}

async fn read_quads<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Vec<Rect> {
    let nums = read_numbers(src, obj).await;
    nums.as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            let xs = [c[0], c[2], c[4], c[6]];
            let ys = [c[1], c[3], c[5], c[7]];
            Rect {
                x0: xs.into_iter().fold(f32::INFINITY, f32::min),
                y0: ys.into_iter().fold(f32::INFINITY, f32::min),
                x1: xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
                y1: ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
            }
        })
        .filter(|r| r.x1 > r.x0 && r.y1 > r.y0)
        .collect()
}

async fn read_numbers<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Vec<f32> {
    let Some(obj) = obj else {
        return Vec::new();
    };
    let Ok(resolved) = src.resolve(obj).await else {
        return Vec::new();
    };
    let Some(arr) = resolved.as_array() else {
        return Vec::new();
    };
    let mut nums = Vec::with_capacity(arr.len());
    for item in arr {
        if let Ok(n) = src.resolve(item).await {
            if let Some(v) = n.as_f64() {
                nums.push(v as f32);
            }
        }
    }
    nums
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yellow_is_a_highlight_fill_black_is_not() {
        assert!(is_highlight_fill(1.0, 1.0, 0.0));
        assert!(is_highlight_fill(0.95, 0.95, 0.5));
        assert!(!is_highlight_fill(0.0, 0.0, 0.0));
        assert!(!is_highlight_fill(1.0, 1.0, 1.0));
    }

    #[test]
    fn highlight_size_rejects_hairlines_and_washes() {
        let page = 612.0;
        assert!(!is_highlight_size(
            Rect {
                x0: 72.0,
                y0: 700.0,
                x1: 172.0,
                y1: 700.5
            },
            page
        ));
        assert!(is_highlight_size(
            Rect {
                x0: 72.0,
                y0: 710.0,
                x1: 160.0,
                y1: 722.0
            },
            page
        ));
        assert!(!is_highlight_size(
            Rect {
                x0: 0.0,
                y0: 710.0,
                x1: 500.0,
                y1: 722.0
            },
            page
        ));
    }
}
