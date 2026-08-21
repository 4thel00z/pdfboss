//! Color spaces: DeviceGray/RGB/CMYK, Indexed, and approximations for the
//! CIE-based and tint-transform families, converted to RGB.

use std::sync::Arc;

use pdfboss_core::{block_on, AsyncObjectSource, Document, Error, Immediate, Object};

use crate::shading::{load_functions, Functions, MAX_COMPS};

/// Nesting guard for color-space definitions that defer to another one
/// (Indexed bases, ICCBased `/Alternate` chains): levels `0..=MAX_DEPTH`
/// are read, anything deeper reads as `DeviceGray`.
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
    /// A one-component `Separation`: its tint transform and the alternate
    /// space the transform's output is read in (§8.6.6.4).
    ///
    /// The transform is evaluated per colour, not per parse: a fill or stroke
    /// costs one evaluation, and an image's samples reach it through the
    /// reader's own per-value table (one entry per distinct sample, 256 at
    /// eight bits) rather than once per pixel. `Arc` so cloning a space
    /// shares one arena instead of copying a sampled function's data.
    Separation {
        tint: Arc<Functions>,
        alternate: Box<ColorSpace>,
    },
    /// Any other family, kept only for its component count. `to_rgb`
    /// approximates it as an ink tint: gray = 1 - max component (used for
    /// `DeviceN`, whose multi-input transforms this does not evaluate, for a
    /// `Separation` whose transform is a type 4, and for Lab).
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
            ColorSpace::Separation { .. } => 1,
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
            ColorSpace::Separation { tint, alternate } => {
                let mut components = [0f32; MAX_COMPS];
                let written = tint.eval(comp(comps, 0), &mut components);
                alternate.to_rgb(&components[..written])
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
        block_on(Self::parse_with(&Immediate(doc), obj))
    }

    /// [`ColorSpace::parse`] against any object source; the synchronous form
    /// is this one over [`Immediate`].
    ///
    /// A loop, not a recursion, and that is load-bearing: a recursive
    /// `async fn` must box itself, and the boxed future poisons the auto-trait
    /// inference the shared executor depends on. The recursion this replaced
    /// was linear — every level defers to at most one other space (an
    /// `ICCBased` `/Alternate`, or an `/Indexed` base) — so one iteration per
    /// level covers it. Descending through `/Indexed` pushes its palette;
    /// whatever the loop finally resolves to gets wrapped in the pending
    /// palettes, innermost last.
    pub(crate) async fn parse_with<S: AsyncObjectSource>(src: &S, obj: &Object) -> ColorSpace {
        let mut palettes: Vec<Vec<u8>> = Vec::new();
        // Tint transforms met on the way down, innermost last — the same
        // deferral `palettes` uses, for the same reason: the transform's
        // output is read in the alternate space, which this loop has not
        // resolved yet.
        let mut tints: Vec<Functions> = Vec::new();
        let mut current: Object = obj.clone();
        // If the loop runs out of levels while still descending, this is the
        // answer — the same one the recursion's depth guard gave.
        let mut result = ColorSpace::DeviceGray;
        for _ in 0..=MAX_DEPTH {
            let resolved = src.resolve(&current).await.unwrap_or(Object::Null);
            match &resolved {
                Object::Name(n) => {
                    result = Self::from_name(&n.0);
                    break;
                }
                Object::Array(items) if !items.is_empty() => {
                    let family = match src.resolve(&items[0]).await {
                        Ok(Object::Name(n)) => n.0,
                        _ => break, // stays DeviceGray
                    };
                    match family.as_str() {
                        "ICCBased" => {
                            let stream = match items.get(1) {
                                Some(o) => src.resolve(o).await.ok(),
                                None => None,
                            };
                            if let Some(Object::Stream(s)) = stream {
                                if let Some(n) = s.dict.get_int("N") {
                                    result = match n {
                                        1 => ColorSpace::DeviceGray,
                                        3 => ColorSpace::DeviceRGB,
                                        4 => ColorSpace::DeviceCMYK,
                                        n => ColorSpace::Other(n.clamp(1, 32) as usize),
                                    };
                                    break;
                                }
                                if let Some(alt) = s.dict.get("Alternate") {
                                    current = alt.clone();
                                    continue;
                                }
                            }
                            result = ColorSpace::DeviceRGB;
                            break;
                        }
                        f if Self::is_indexed(f) => {
                            let lookup = match items.get(3) {
                                Some(o) => match src.resolve(o).await {
                                    Ok(Object::String(bytes)) => bytes,
                                    Ok(Object::Stream(s)) => {
                                        src.stream_data(&s).await.unwrap_or_default()
                                    }
                                    _ => Vec::new(),
                                },
                                None => Vec::new(),
                            };
                            palettes.push(lookup);
                            match items.get(1) {
                                Some(base) => {
                                    current = base.clone();
                                    continue;
                                }
                                None => break, // base stays DeviceGray
                            }
                        }
                        "Separation" => {
                            // [/Separation name alternate transform]: keep the
                            // transform and carry on into the alternate space,
                            // whose components the transform's output *is*.
                            let transform = match items.get(3) {
                                Some(o) => load_functions(src, o).await.ok().flatten(),
                                None => None,
                            };
                            match (transform, items.get(2)) {
                                (Some(funcs), Some(alternate)) => {
                                    tints.push(funcs);
                                    current = alternate.clone();
                                    continue;
                                }
                                // A type 4 transform, a malformed one, or no
                                // alternate at all: the ink approximation is
                                // still better than painting nothing.
                                _ => {
                                    result = ColorSpace::Other(1);
                                    break;
                                }
                            }
                        }
                        "DeviceN" => {
                            let n = match items.get(1) {
                                Some(o) => match src.resolve(o).await {
                                    Ok(Object::Array(names)) => names.len().max(1),
                                    _ => 1,
                                },
                                None => 1,
                            };
                            result = ColorSpace::Other(n);
                            break;
                        }
                        other => {
                            result = Self::from_name(other);
                            break;
                        }
                    }
                }
                _ => break, // stays DeviceGray
            }
        }
        for funcs in tints.into_iter().rev() {
            result = ColorSpace::Separation {
                tint: Arc::new(funcs),
                alternate: Box::new(result),
            };
        }
        for lookup in palettes.into_iter().rev() {
            result = ColorSpace::Indexed {
                base: Box::new(result),
                lookup,
            };
        }
        result
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

    /// Whether this is an `/Indexed` space, given the family name of an
    /// array-form color space (`I` is the inline-image abbreviation).
    fn is_indexed(family: &str) -> bool {
        matches!(family, "Indexed" | "I")
    }
}

/// The decode failure behind an empty `/Indexed` palette, if the space is
/// `/Indexed` and its lookup table is a stream that will not decode.
///
/// [`ColorSpace::parse`] is lenient and leaves such a space with no palette
/// at all, which paints every sample black. That looks like a decoded image
/// but is not one, so the executor asks here and reports it instead.
pub(crate) fn palette_error(doc: &Document, obj: &Object) -> Option<Error> {
    block_on(palette_error_with(&Immediate(doc), obj))
}

/// [`palette_error`] against any object source; the synchronous form is this
/// one over [`Immediate`].
pub(crate) async fn palette_error_with<S: AsyncObjectSource>(
    src: &S,
    obj: &Object,
) -> Option<Error> {
    let Ok(Object::Array(items)) = src.resolve(obj).await else {
        return None;
    };
    let Ok(Object::Name(family)) = src.resolve(items.first()?).await else {
        return None;
    };
    if !ColorSpace::is_indexed(&family.0) {
        return None;
    }
    match src.resolve(items.get(3)?).await {
        Ok(Object::Stream(s)) => src.stream_data(&s).await.err(),
        _ => None,
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

    /// An `/Indexed` space whose base is itself `/Indexed`. Each level pushes a
    /// palette on the way down, and the wrap on the way out must apply them
    /// innermost-last: the outer palette's single entry selects the inner
    /// palette's red.
    #[test]
    fn a_nested_indexed_base_wraps_in_definition_order() {
        let doc = test_doc();
        let cs = ColorSpace::parse(
            &doc,
            &obj(b"[/Indexed [/Indexed /DeviceRGB 0 <FF0000>] 0 <00>]"),
        );
        assert_eq!(cs.to_rgb(&[0.0]), [1.0, 0.0, 0.0]);
    }

    /// An `ICCBased` stream with no `/N` whose `/Alternate` refers back to the
    /// same space. The nesting guard has to terminate this and read it as
    /// gray; without the guard this loops forever on a two-object file.
    #[test]
    fn a_circular_alternate_chain_terminates_as_gray() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.object(7, "[/ICCBased 8 0 R]");
        b.stream(8, "/Alternate 7 0 R", b"");
        let doc = Document::load(b.build(1)).unwrap();
        let cs = ColorSpace::parse(&doc, &obj(b"7 0 R"));
        assert_eq!(cs, ColorSpace::DeviceGray);
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
