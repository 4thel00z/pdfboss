//! Color spaces: DeviceGray/RGB/CMYK, Indexed, and approximations for the
//! CIE-based and tint-transform families, converted to RGB.

use pdfboss_core::{Document, Object};

/// Recursion guard for nested color-space definitions (Indexed bases).
const MAX_DEPTH: u32 = 8;

/// A color space reduced to what the rasterizer can paint.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ColorSpace {
    /// One gray component.
    DeviceGray,
    /// Red, green, blue.
    DeviceRGB,
    /// Cyan, magenta, yellow, black.
    DeviceCMYK,
    /// Palette lookup: the single component indexes `lookup`, whose bytes
    /// are `base` components scaled to 0..=255.
    Indexed {
        base: Box<ColorSpace>,
        lookup: Vec<u8>,
    },
    /// Any other family, kept only for its component count. `to_rgb`
    /// approximates it as an ink tint: gray = 1 - max component (used for
    /// Separation/DeviceN, whose tint transforms are not evaluated, and
    /// Lab).
    Other(usize),
}

/// Fetches component `i`, defaulting to 0 and clamping to 0..=1
/// (non-finite values become 0).
fn comp(comps: &[f32], i: usize) -> f32 {
    let v = comps.get(i).copied().unwrap_or(0.0);
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

impl ColorSpace {
    /// Number of color components an operand for this space carries.
    pub(crate) fn components(&self) -> usize {
        match self {
            ColorSpace::DeviceGray => 1,
            ColorSpace::DeviceRGB => 3,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::Indexed { .. } => 1,
            ColorSpace::Other(n) => *n,
        }
    }

    /// Converts component values to RGB in 0..=1. Missing components read
    /// as 0; out-of-range and non-finite values are clamped.
    pub(crate) fn to_rgb(&self, comps: &[f32]) -> [f32; 3] {
        match self {
            ColorSpace::DeviceGray => {
                let g = comp(comps, 0);
                [g, g, g]
            }
            ColorSpace::DeviceRGB => [comp(comps, 0), comp(comps, 1), comp(comps, 2)],
            ColorSpace::DeviceCMYK => {
                let k = comp(comps, 3);
                [
                    1.0 - (comp(comps, 0) + k).min(1.0),
                    1.0 - (comp(comps, 1) + k).min(1.0),
                    1.0 - (comp(comps, 2) + k).min(1.0),
                ]
            }
            ColorSpace::Indexed { base, lookup } => {
                let n = base.components().max(1);
                let count = lookup.len() / n;
                if count == 0 {
                    return [0.0, 0.0, 0.0];
                }
                let raw = comps.first().copied().unwrap_or(0.0);
                let idx = if raw.is_finite() {
                    (raw.round().max(0.0) as usize).min(count - 1)
                } else {
                    0
                };
                let mut base_comps = [0.0f32; 8];
                let n = n.min(8);
                for (i, bc) in base_comps.iter_mut().take(n).enumerate() {
                    *bc = lookup[idx * base.components() + i] as f32 / 255.0;
                }
                base.to_rgb(&base_comps[..n])
            }
            ColorSpace::Other(n) => {
                // Tint approximation: treat the strongest component as ink
                // coverage v and paint gray 1 - v.
                let tint = (0..*n).map(|i| comp(comps, i)).fold(0.0f32, f32::max);
                let g = 1.0 - tint;
                [g, g, g]
            }
        }
    }

    /// Parses a color-space object from a resource dictionary. Lenient:
    /// anything unrecognized falls back to `DeviceGray`. `ICCBased` maps by
    /// `/N` (or its `/Alternate`), the CIE `Cal*` families map to their
    /// device equivalents, `Lab` keeps its 3 components as [`Other`],
    /// and `Separation`/`DeviceN` become [`Other`] with the documented
    /// tint approximation (their tint transforms are not evaluated).
    ///
    /// [`Other`]: ColorSpace::Other
    pub(crate) fn parse(doc: &Document, obj: &Object) -> ColorSpace {
        Self::parse_at(doc, obj, 0)
    }

    fn parse_at(doc: &Document, obj: &Object, depth: u32) -> ColorSpace {
        if depth > MAX_DEPTH {
            return ColorSpace::DeviceGray;
        }
        let obj = doc.resolve(obj).unwrap_or(Object::Null);
        match &obj {
            Object::Name(n) => Self::from_name(&n.0),
            Object::Array(items) if !items.is_empty() => {
                let family = match doc.resolve(&items[0]) {
                    Ok(Object::Name(n)) => n.0,
                    _ => return ColorSpace::DeviceGray,
                };
                match family.as_str() {
                    "ICCBased" => Self::parse_icc(doc, items.get(1), depth),
                    "Indexed" | "I" => Self::parse_indexed(doc, items, depth),
                    "Separation" => ColorSpace::Other(1),
                    "DeviceN" => {
                        let n = match items.get(1).map(|o| doc.resolve(o)) {
                            Some(Ok(Object::Array(names))) => names.len().max(1),
                            _ => 1,
                        };
                        ColorSpace::Other(n)
                    }
                    other => Self::from_name(other),
                }
            }
            _ => ColorSpace::DeviceGray,
        }
    }

    /// Maps a bare color-space name (including the inline-image
    /// abbreviations) to a color space.
    fn from_name(name: &str) -> ColorSpace {
        match name {
            "DeviceRGB" | "RGB" | "CalRGB" => ColorSpace::DeviceRGB,
            "DeviceCMYK" | "CMYK" => ColorSpace::DeviceCMYK,
            "Lab" => ColorSpace::Other(3),
            // DeviceGray, G, CalGray; Pattern paints mid-gray via the
            // executor, so a 1-component gray placeholder suffices.
            _ => ColorSpace::DeviceGray,
        }
    }

    fn parse_icc(doc: &Document, stream_obj: Option<&Object>, depth: u32) -> ColorSpace {
        let resolved = stream_obj.map(|o| doc.resolve(o));
        if let Some(Ok(Object::Stream(s))) = resolved {
            if let Some(n) = s.dict.get_int("N") {
                return match n {
                    1 => ColorSpace::DeviceGray,
                    3 => ColorSpace::DeviceRGB,
                    4 => ColorSpace::DeviceCMYK,
                    n => ColorSpace::Other(n.clamp(1, 32) as usize),
                };
            }
            if let Some(alt) = s.dict.get("Alternate") {
                return Self::parse_at(doc, alt, depth + 1);
            }
        }
        ColorSpace::DeviceRGB
    }

    fn parse_indexed(doc: &Document, items: &[Object], depth: u32) -> ColorSpace {
        let base = match items.get(1) {
            Some(b) => Self::parse_at(doc, b, depth + 1),
            None => ColorSpace::DeviceGray,
        };
        let lookup = match items.get(3).map(|o| doc.resolve(o)) {
            Some(Ok(Object::String(bytes))) => bytes,
            Some(Ok(Object::Stream(s))) => doc.stream_data(&s).unwrap_or_default(),
            _ => Vec::new(),
        };
        ColorSpace::Indexed {
            base: Box::new(base),
            lookup,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::parser::{NoResolve, Parser};
    use pdfboss_testkit::PdfBuilder;

    fn test_doc() -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(4, "/N 4", b"");
        b.stream(6, "", &[10, 200, 30, 250]);
        Document::load(b.build(1)).unwrap()
    }

    fn obj(src: &[u8]) -> Object {
        Parser::new(src).parse_object(&NoResolve).unwrap()
    }

    #[test]
    fn gray_and_rgb_to_rgb() {
        assert_eq!(ColorSpace::DeviceGray.to_rgb(&[0.25]), [0.25, 0.25, 0.25]);
        assert_eq!(
            ColorSpace::DeviceRGB.to_rgb(&[0.1, 0.5, 0.9]),
            [0.1, 0.5, 0.9]
        );
        // Missing components default to 0; out-of-range values clamp.
        assert_eq!(ColorSpace::DeviceRGB.to_rgb(&[2.0]), [1.0, 0.0, 0.0]);
        assert_eq!(ColorSpace::DeviceGray.to_rgb(&[f32::NAN]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn cmyk_to_rgb_naive() {
        assert_eq!(
            ColorSpace::DeviceCMYK.to_rgb(&[1.0, 0.0, 0.0, 0.0]),
            [0.0, 1.0, 1.0]
        );
        assert_eq!(
            ColorSpace::DeviceCMYK.to_rgb(&[0.0, 0.0, 0.0, 1.0]),
            [0.0, 0.0, 0.0]
        );
        let [r, g, b] = ColorSpace::DeviceCMYK.to_rgb(&[0.5, 0.2, 0.0, 0.3]);
        assert!((r - 0.2).abs() < 1e-6);
        assert!((g - 0.5).abs() < 1e-6);
        assert!((b - 0.7).abs() < 1e-6);
    }

    #[test]
    fn indexed_lookup_and_out_of_range_clamp() {
        let cs = ColorSpace::Indexed {
            base: Box::new(ColorSpace::DeviceRGB),
            lookup: vec![255, 0, 0, 0, 255, 0, 0, 0, 255],
        };
        assert_eq!(cs.components(), 1);
        assert_eq!(cs.to_rgb(&[1.0]), [0.0, 1.0, 0.0]);
        // Out-of-range indices clamp to the palette bounds.
        assert_eq!(cs.to_rgb(&[9.0]), [0.0, 0.0, 1.0]);
        assert_eq!(cs.to_rgb(&[-3.0]), [1.0, 0.0, 0.0]);
        // Empty palette stays black instead of panicking.
        let empty = ColorSpace::Indexed {
            base: Box::new(ColorSpace::DeviceRGB),
            lookup: Vec::new(),
        };
        assert_eq!(empty.to_rgb(&[0.0]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn other_uses_tint_approximation() {
        assert_eq!(ColorSpace::Other(1).to_rgb(&[0.25]), [0.75, 0.75, 0.75]);
        // The strongest component wins: 1 - max(0.1, 0.9) = 0.1.
        let g = ColorSpace::Other(2).to_rgb(&[0.1, 0.9]);
        assert!((g[0] - 0.1).abs() < 1e-5, "gray {g:?}");
        assert_eq!(g[0], g[1]);
        assert_eq!(g[1], g[2]);
        assert_eq!(ColorSpace::Other(1).to_rgb(&[]), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn component_counts() {
        assert_eq!(ColorSpace::DeviceGray.components(), 1);
        assert_eq!(ColorSpace::DeviceRGB.components(), 3);
        assert_eq!(ColorSpace::DeviceCMYK.components(), 4);
        assert_eq!(ColorSpace::Other(5).components(), 5);
    }

    #[test]
    fn parse_names_and_abbreviations() {
        let doc = test_doc();
        let p = |s: &[u8]| ColorSpace::parse(&doc, &obj(s));
        assert_eq!(p(b"/DeviceGray"), ColorSpace::DeviceGray);
        assert_eq!(p(b"/G"), ColorSpace::DeviceGray);
        assert_eq!(p(b"/DeviceRGB"), ColorSpace::DeviceRGB);
        assert_eq!(p(b"/RGB"), ColorSpace::DeviceRGB);
        assert_eq!(p(b"/DeviceCMYK"), ColorSpace::DeviceCMYK);
        assert_eq!(p(b"/CalRGB"), ColorSpace::DeviceRGB);
        assert_eq!(p(b"/CalGray"), ColorSpace::DeviceGray);
        assert_eq!(p(b"/Lab"), ColorSpace::Other(3));
        assert_eq!(p(b"/NoSuchSpace"), ColorSpace::DeviceGray);
        assert_eq!(p(b"42"), ColorSpace::DeviceGray);
    }

    #[test]
    fn parse_array_families() {
        let doc = test_doc();
        let p = |s: &[u8]| ColorSpace::parse(&doc, &obj(s));
        assert_eq!(p(b"[/ICCBased 4 0 R]"), ColorSpace::DeviceCMYK);
        assert_eq!(
            p(b"[/CalRGB << /WhitePoint [1 1 1] >>]"),
            ColorSpace::DeviceRGB
        );
        assert_eq!(
            p(b"[/CalGray << /WhitePoint [1 1 1] >>]"),
            ColorSpace::DeviceGray
        );
        assert_eq!(p(b"[/Lab << /WhitePoint [1 1 1] >>]"), ColorSpace::Other(3));
        assert_eq!(
            p(b"[/Separation /Spot /DeviceCMYK 4 0 R]"),
            ColorSpace::Other(1)
        );
        assert_eq!(
            p(b"[/DeviceN [/A /B /C] /DeviceRGB 4 0 R]"),
            ColorSpace::Other(3)
        );
        assert_eq!(p(b"[/DeviceRGB]"), ColorSpace::DeviceRGB);
        assert_eq!(p(b"[]"), ColorSpace::DeviceGray);
        // A missing ICC stream falls back to 3-component RGB.
        assert_eq!(p(b"[/ICCBased 99 0 R]"), ColorSpace::DeviceRGB);
    }

    #[test]
    fn parse_indexed_with_string_lookup() {
        let doc = test_doc();
        let cs = ColorSpace::parse(&doc, &obj(b"[/Indexed /DeviceRGB 2 <FF000000FF000000FF>]"));
        match &cs {
            ColorSpace::Indexed { base, lookup } => {
                assert_eq!(**base, ColorSpace::DeviceRGB);
                assert_eq!(lookup.len(), 9);
            }
            other => panic!("expected Indexed, got {other:?}"),
        }
        assert_eq!(cs.to_rgb(&[2.0]), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn parse_indexed_with_stream_lookup() {
        let doc = test_doc();
        let cs = ColorSpace::parse(&doc, &obj(b"[/Indexed /DeviceGray 3 6 0 R]"));
        match &cs {
            ColorSpace::Indexed { base, lookup } => {
                assert_eq!(**base, ColorSpace::DeviceGray);
                assert_eq!(lookup, &vec![10, 200, 30, 250]);
            }
            other => panic!("expected Indexed, got {other:?}"),
        }
        let [r, _, _] = cs.to_rgb(&[1.0]);
        assert!((r - 200.0 / 255.0).abs() < 1e-5);
    }
}
