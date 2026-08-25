//! Color spaces: DeviceGray/RGB/CMYK, Indexed, embedded ICC profiles, the
//! CIE-based families (CalRGB, CalGray, Lab), and Separation/DeviceN
//! through their tint transforms,
//! converted to RGB.

use std::sync::Arc;

use pdfboss_core::{block_on, AsyncObjectSource, Dict, Document, Error, Immediate, Object, Stream};
use pdfboss_icc::{
    lab_to_xyz, mat_apply, mat_mul, srgb_encode, xyz_to_linear_srgb, DeviceSpace, Mat3, Profile,
};

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
    /// A `Separation` or `DeviceN` space (§8.6.6.4/§8.6.6.5): `inputs` tint
    /// components run through the transform, whose outputs are read in the
    /// alternate space. Separation is the one-input case.
    ///
    /// The transform is evaluated per colour, not per parse: a fill or stroke
    /// costs one evaluation, and a one-component image's samples reach it
    /// through the reader's per-value table (one entry per distinct sample,
    /// 256 at eight bits) rather than once per pixel; multi-colorant DeviceN
    /// images pay one evaluation per pixel. `Arc` so cloning a space shares
    /// one arena instead of copying a sampled function's data.
    Separation {
        tint: Arc<Functions>,
        alternate: Box<ColorSpace>,
        inputs: usize,
    },
    /// An `ICCBased` space whose profile parsed and is not equivalent to a
    /// device space: `n` components map through the profile to sRGB.
    Icc { profile: Arc<Profile>, n: usize },
    /// `CalRGB` (§8.6.5.3): per-channel gamma, then a matrix taking the
    /// space's XYZ to linear sRGB (whitepoint adaptation folded in).
    CalRgb { gamma: [f32; 3], m: Mat3 },
    /// `CalGray` (§8.6.5.2): gray decodes to the whitepoint scaled by
    /// A^gamma, and the whitepoint cancels through adaptation, leaving the
    /// sRGB encoding of A^gamma.
    CalGray { gamma: f32 },
    /// `Lab` (§8.6.5.4): L*a*b* against `white`, with a*/b* clamped to
    /// `range`, converted to XYZ and through `m` to linear sRGB.
    Lab {
        m: Mat3,
        white: [f32; 3],
        range: [f32; 4],
    },
    /// Any other family, kept only for its component count. `to_rgb`
    /// approximates it as an ink tint: gray = 1 - max component (used for
    /// a `Separation`/`DeviceN` whose transform will not load, and for
    /// `DeviceN` beyond 8 colorants).
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
            ColorSpace::Separation { inputs, .. } => *inputs,
            ColorSpace::Icc { n, .. } => *n,
            ColorSpace::CalRgb { .. } => 3,
            ColorSpace::CalGray { .. } => 1,
            ColorSpace::Lab { .. } => 3,
            ColorSpace::Other(n) => *n,
        }
    }

    /// The default per-component sample range: `/Decode` defaults for
    /// images, and the range `/Indexed` palette bytes rescale into. `max`
    /// is the largest raw sample value.
    pub(crate) fn default_decode(&self, c: usize, max: f32) -> (f32, f32) {
        match self {
            ColorSpace::Indexed { .. } => (0.0, max),
            ColorSpace::Lab { range, .. } => match c {
                0 => (0.0, 100.0),
                1 => (range[0], range[1]),
                _ => (range[2], range[3]),
            },
            _ => (0.0, 1.0),
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
                    let (lo, hi) = base.default_decode(i, 255.0);
                    *bc = lo + lookup[idx * base.components() + i] as f32 * (hi - lo) / 255.0;
                }
                base.to_rgb(&base_comps[..n])
            }
            ColorSpace::Separation {
                tint,
                alternate,
                inputs,
            } => {
                let mut tints = [0f32; MAX_COMPS];
                for (i, slot) in tints.iter_mut().enumerate().take(*inputs) {
                    *slot = comp(comps, i);
                }
                let mut components = [0f32; MAX_COMPS];
                let written = tint.eval(&tints[..*inputs], &mut components);
                alternate.to_rgb(&components[..written])
            }
            ColorSpace::Icc { profile, n } => {
                let mut vals = [0.0f32; MAX_COMPS];
                for (i, slot) in vals.iter_mut().take(*n).enumerate() {
                    *slot = comp(comps, i);
                }
                profile.transform(&vals[..*n])
            }
            ColorSpace::CalRgb { gamma, m } => {
                let lin = [
                    comp(comps, 0).powf(gamma[0]),
                    comp(comps, 1).powf(gamma[1]),
                    comp(comps, 2).powf(gamma[2]),
                ];
                let rgb = mat_apply(m, lin);
                [
                    srgb_encode(rgb[0]),
                    srgb_encode(rgb[1]),
                    srgb_encode(rgb[2]),
                ]
            }
            ColorSpace::CalGray { gamma } => {
                let v = srgb_encode(comp(comps, 0).powf(*gamma));
                [v, v, v]
            }
            ColorSpace::Lab { m, white, range } => {
                let raw = |i: usize| -> f32 {
                    let v = comps.get(i).copied().unwrap_or(0.0);
                    if v.is_finite() {
                        v
                    } else {
                        0.0
                    }
                };
                let lab = [
                    raw(0).clamp(0.0, 100.0),
                    raw(1).clamp(range[0], range[1]),
                    raw(2).clamp(range[2], range[3]),
                ];
                let rgb = mat_apply(m, lab_to_xyz(lab, *white));
                [
                    srgb_encode(rgb[0]),
                    srgb_encode(rgb[1]),
                    srgb_encode(rgb[2]),
                ]
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
    /// anything unrecognized falls back to `DeviceGray`. `ICCBased`
    /// decodes and applies its profile (device-equivalent profiles map to
    /// their device space; a stream that will not parse falls back to the
    /// `/N` reduction or its `/Alternate`), the CIE families `CalRGB`,
    /// `CalGray`, and `Lab` convert through XYZ, and `Separation`/`DeviceN`
    /// evaluate their tint transforms into the alternate space — falling
    /// back to [`Other`] with the documented ink approximation when the
    /// transform will not load or a `DeviceN` names more than 8 colorants.
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
        // Tint transforms met on the way down with their input arity,
        // innermost last — the same deferral `palettes` uses, for the same
        // reason: the transform's output is read in the alternate space,
        // which this loop has not resolved yet.
        let mut tints: Vec<(Functions, usize)> = Vec::new();
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
                                if let Some(cs) = Self::from_icc(src, &s).await {
                                    result = cs;
                                    break;
                                }
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
                                Some(o) => load_functions(src, o).await.ok(),
                                None => None,
                            };
                            match (transform, items.get(2)) {
                                (Some(funcs), Some(alternate)) => {
                                    tints.push((funcs, 1));
                                    current = alternate.clone();
                                    continue;
                                }
                                // A malformed transform or no alternate at
                                // all: the ink approximation is still better
                                // than painting nothing.
                                _ => {
                                    result = ColorSpace::Other(1);
                                    break;
                                }
                            }
                        }
                        "CalRGB" => {
                            result = Self::cal_rgb(src, items.get(1)).await;
                            break;
                        }
                        "CalGray" => {
                            result = Self::cal_gray(src, items.get(1)).await;
                            break;
                        }
                        "Lab" => {
                            result = Self::lab(src, items.get(1)).await;
                            break;
                        }
                        "DeviceN" => {
                            // [/DeviceN names alternate transform]: Separation
                            // with one tint per colorant (§8.6.6.5), deferred
                            // the same way.
                            let n = match items.get(1) {
                                Some(o) => match src.resolve(o).await {
                                    Ok(Object::Array(names)) => names.len().max(1),
                                    _ => 1,
                                },
                                None => 1,
                            };
                            let transform = match items.get(3) {
                                Some(o) if n <= MAX_COMPS => load_functions(src, o).await.ok(),
                                _ => None,
                            };
                            match (transform, items.get(2)) {
                                (Some(funcs), Some(alternate)) => {
                                    tints.push((funcs, n));
                                    current = alternate.clone();
                                    continue;
                                }
                                // More colorants than the pipeline carries,
                                // or a transform that will not load: the ink
                                // approximation.
                                _ => {
                                    result = ColorSpace::Other(n);
                                    break;
                                }
                            }
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
        for (funcs, inputs) in tints.into_iter().rev() {
            result = ColorSpace::Separation {
                tint: Arc::new(funcs),
                alternate: Box::new(result),
                inputs,
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

    /// Decodes and parses an `ICCBased` stream's profile. `None` — the
    /// stream will not decode, the profile will not parse, its arity
    /// disagrees with a present `/N` or exceeds [`MAX_COMPS`] — falls back
    /// to the caller's `/N` reduction. A profile equivalent to a device
    /// space maps straight to it, so sRGB-wrapping files keep their exact
    /// device-RGB output.
    async fn from_icc<S: AsyncObjectSource>(src: &S, s: &Stream) -> Option<ColorSpace> {
        let data = src.stream_data(s).await.ok()?;
        let profile = pdfboss_icc::parse(&data).ok()?;
        let n = profile.channels();
        if n > MAX_COMPS {
            return None;
        }
        if let Some(declared) = s.dict.get_int("N") {
            if declared != n as i64 {
                return None;
            }
        }
        match profile.device_equivalent() {
            Some(DeviceSpace::Rgb) => Some(ColorSpace::DeviceRGB),
            Some(DeviceSpace::Gray) => Some(ColorSpace::DeviceGray),
            None => Some(ColorSpace::Icc {
                profile: Arc::new(profile),
                n,
            }),
        }
    }

    /// Resolves `obj` to an array of at least `want` finite numbers.
    async fn numbers<S: AsyncObjectSource>(
        src: &S,
        obj: Option<&Object>,
        want: usize,
    ) -> Option<Vec<f32>> {
        let Ok(Object::Array(items)) = src.resolve(obj?).await else {
            return None;
        };
        if items.len() < want {
            return None;
        }
        let mut out = Vec::with_capacity(want);
        for item in items.iter().take(want) {
            let v = src.resolve(item).await.ok()?.as_f64()?;
            if !v.is_finite() {
                return None;
            }
            out.push(v as f32);
        }
        Some(out)
    }

    /// The required, all-positive `/WhitePoint` of a CIE space dictionary.
    async fn white_point<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<[f32; 3]> {
        let wp = Self::numbers(src, dict.get("WhitePoint"), 3).await?;
        if wp.iter().any(|&v| v <= 0.0) {
            return None;
        }
        Some([wp[0], wp[1], wp[2]])
    }

    async fn cie_dict<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Option<Dict> {
        match src.resolve(obj?).await {
            Ok(Object::Dict(dict)) => Some(dict),
            _ => None,
        }
    }

    /// `[/CalRGB dict]`: gamma defaults to 1 per channel, `/Matrix` to the
    /// identity (§8.6.5.3). A missing or invalid `/WhitePoint` keeps the
    /// old DeviceRGB reading.
    async fn cal_rgb<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> ColorSpace {
        let Some(dict) = Self::cie_dict(src, obj).await else {
            return ColorSpace::DeviceRGB;
        };
        let Some(white) = Self::white_point(src, &dict).await else {
            return ColorSpace::DeviceRGB;
        };
        let gamma = match Self::numbers(src, dict.get("Gamma"), 3).await {
            Some(g) => [g[0], g[1], g[2]],
            None => [1.0; 3],
        };
        let cal = match Self::numbers(src, dict.get("Matrix"), 9).await {
            // /Matrix is column-major: [X_A Y_A Z_A X_B ...].
            Some(v) => [[v[0], v[3], v[6]], [v[1], v[4], v[7]], [v[2], v[5], v[8]]],
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        ColorSpace::CalRgb {
            gamma,
            m: mat_mul(&xyz_to_linear_srgb(white), &cal),
        }
    }

    /// `[/CalGray dict]` (§8.6.5.2); the whitepoint is validated but
    /// cancels out of the conversion.
    async fn cal_gray<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> ColorSpace {
        let Some(dict) = Self::cie_dict(src, obj).await else {
            return ColorSpace::DeviceGray;
        };
        if Self::white_point(src, &dict).await.is_none() {
            return ColorSpace::DeviceGray;
        }
        let gamma = match dict.get("Gamma") {
            Some(o) => match src.resolve(o).await.ok().and_then(|v| v.as_f64()) {
                Some(g) if g.is_finite() && g > 0.0 => g as f32,
                _ => 1.0,
            },
            None => 1.0,
        };
        ColorSpace::CalGray { gamma }
    }

    /// `[/Lab dict]` (§8.6.5.4): `/Range` defaults to ±100 for a* and b*;
    /// an inverted pair falls back to that default.
    async fn lab<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> ColorSpace {
        let Some(dict) = Self::cie_dict(src, obj).await else {
            return ColorSpace::Other(3);
        };
        let Some(white) = Self::white_point(src, &dict).await else {
            return ColorSpace::Other(3);
        };
        let mut range = [-100.0f32, 100.0, -100.0, 100.0];
        if let Some(r) = Self::numbers(src, dict.get("Range"), 4).await {
            if r[0] <= r[1] && r[2] <= r[3] {
                range = [r[0], r[1], r[2], r[3]];
            }
        }
        ColorSpace::Lab {
            m: xyz_to_linear_srgb(white),
            white,
            range,
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
        match p(b"[/CalRGB << /WhitePoint [1 1 1] >>]") {
            ColorSpace::CalRgb { gamma, .. } => assert_eq!(gamma, [1.0; 3]),
            other => panic!("expected CalRgb, got {other:?}"),
        }
        assert_eq!(
            p(b"[/CalGray << /WhitePoint [1 1 1] >>]"),
            ColorSpace::CalGray { gamma: 1.0 }
        );
        assert!(matches!(
            p(b"[/Lab << /WhitePoint [1 1 1] >>]"),
            ColorSpace::Lab { .. }
        ));
        // A CIE dictionary without its required /WhitePoint keeps the old
        // device reading.
        assert_eq!(p(b"[/CalRGB << >>]"), ColorSpace::DeviceRGB);
        assert_eq!(p(b"[/CalGray << >>]"), ColorSpace::DeviceGray);
        assert_eq!(p(b"[/Lab << >>]"), ColorSpace::Other(3));
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

    /// A `/Separation` whose tint transform is a type 4 calculator: the
    /// program maps tint `t` to `(1-t, 0, 0)` in the DeviceRGB alternate.
    #[test]
    fn separation_with_a_calculator_tint_evaluates() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(
            5,
            "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
            b"{ 1 exch sub 0 0 }",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let cs = ColorSpace::parse(&doc, &obj(b"[/Separation /Spot /DeviceRGB 5 0 R]"));
        assert!(matches!(cs, ColorSpace::Separation { .. }), "{cs:?}");
        let [r, g, bl] = cs.to_rgb(&[0.5]);
        assert!((r - 0.5).abs() < 1e-5, "{r}");
        assert_eq!((g, bl), (0.0, 0.0));
    }

    /// DeviceN is Separation with n colorants (§8.6.6.5): every input
    /// reaches the transform, whose outputs are read in the alternate space.
    #[test]
    fn devicen_evaluates_its_multi_input_tint_transform() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(
            5,
            "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
            b"{ 0 }",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let cs = ColorSpace::parse(&doc, &obj(b"[/DeviceN [/A /B] /DeviceRGB 5 0 R]"));
        assert_eq!(cs.components(), 2);
        let [r, g, bl] = cs.to_rgb(&[0.2, 0.6]);
        assert!((r - 0.2).abs() < 1e-5, "{r}");
        assert!((g - 0.6).abs() < 1e-5, "{g}");
        assert_eq!(bl, 0.0);
    }

    /// The same seam over a two-input sampled grid: the tint pair lands in
    /// the gray computed by the bilinear blend of the 2x2 table.
    #[test]
    fn devicen_evaluates_a_sampled_grid_tint() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(
            5,
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1] /Size [2 2] /BitsPerSample 8",
            &[0, 100, 200, 255],
        );
        let doc = Document::load(b.build(1)).unwrap();
        let cs = ColorSpace::parse(&doc, &obj(b"[/DeviceN [/A /B] /DeviceGray 5 0 R]"));
        let [r, g, bl] = cs.to_rgb(&[0.5, 0.5]);
        assert!((r - 138.75 / 255.0).abs() < 1e-4, "{r}");
        assert_eq!(r, g);
        assert_eq!(g, bl);
    }

    /// More colorants than the pipeline carries stay on the documented
    /// ink approximation.
    #[test]
    fn devicen_beyond_eight_colorants_keeps_the_ink_approximation() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(5, "/FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1", b"");
        let doc = Document::load(b.build(1)).unwrap();
        let cs = ColorSpace::parse(
            &doc,
            &obj(b"[/DeviceN [/A /B /C /D /E /F /G /H /I] /DeviceGray 5 0 R]"),
        );
        assert_eq!(cs, ColorSpace::Other(9));
    }

    fn fx(v: f64) -> [u8; 4] {
        (((v * 65536.0).round()) as i32).to_be_bytes()
    }

    /// A minimal matrix/TRC RGB profile: the sRGB colorant columns in the
    /// D50 PCS plus one shared TRC tag.
    fn rgb_profile(trc: &[u8]) -> Vec<u8> {
        let columns: [[f64; 3]; 3] = [
            [0.4360, 0.2225, 0.0139],
            [0.3851, 0.7169, 0.0971],
            [0.1431, 0.0606, 0.7139],
        ];
        let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for (sig, col) in [*b"rXYZ", *b"gXYZ", *b"bXYZ"].iter().zip(columns) {
            let mut data = b"XYZ \0\0\0\0".to_vec();
            for v in col {
                data.extend_from_slice(&fx(v));
            }
            tags.push((*sig, data));
        }
        for sig in [*b"rTRC", *b"gTRC", *b"bTRC"] {
            tags.push((sig, trc.to_vec()));
        }
        let mut header = vec![0u8; 128];
        header[8] = 4;
        header[16..20].copy_from_slice(b"RGB ");
        header[20..24].copy_from_slice(b"XYZ ");
        header[36..40].copy_from_slice(b"acsp");
        let mut table = (tags.len() as u32).to_be_bytes().to_vec();
        let mut body = Vec::new();
        let mut at = 132 + 12 * tags.len();
        for (sig, data) in &tags {
            table.extend_from_slice(sig);
            table.extend_from_slice(&(at as u32).to_be_bytes());
            table.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            at += data.len();
        }
        let mut out = header;
        out.extend_from_slice(&table);
        out.extend_from_slice(&body);
        let size = (out.len() as u32).to_be_bytes();
        out[0..4].copy_from_slice(&size);
        out
    }

    fn srgb_trc() -> Vec<u8> {
        let mut out = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            out.extend_from_slice(&fx(v));
        }
        out
    }

    fn icc_doc(n: &str, profile: &[u8]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.stream(4, n, profile);
        Document::load(b.build(1)).unwrap()
    }

    /// An `ICCBased` stream wrapping sRGB reads as plain DeviceRGB — the
    /// fast path that keeps such files byte-identical.
    #[test]
    fn an_srgb_icc_stream_maps_to_device_rgb() {
        let doc = icc_doc("/N 3", &rgb_profile(&srgb_trc()));
        let cs = ColorSpace::parse(&doc, &obj(b"[/ICCBased 4 0 R]"));
        assert_eq!(cs, ColorSpace::DeviceRGB);
    }

    /// A gamma-1,8 profile transforms: mid-gray brightens to the sRGB
    /// encoding of 0,5^1,8, and pure inputs stay on their axis.
    #[test]
    fn a_gamma_18_icc_stream_transforms() {
        let mut trc = b"curv\0\0\0\0\0\0\0\x01".to_vec();
        trc.extend_from_slice(&((1.8f64 * 256.0).round() as u16).to_be_bytes());
        let doc = icc_doc("/N 3", &rgb_profile(&trc));
        let cs = ColorSpace::parse(&doc, &obj(b"[/ICCBased 4 0 R]"));
        assert!(matches!(cs, ColorSpace::Icc { n: 3, .. }), "{cs:?}");
        assert_eq!(cs.components(), 3);
        let out = cs.to_rgb(&[0.5, 0.5, 0.5]);
        let want = srgb_encode(0.5f32.powf(1.8));
        for o in out {
            assert!((o - want).abs() < 0.01, "{out:?} want {want}");
        }
    }

    /// A profile whose arity disagrees with `/N` is distrusted: the `/N`
    /// reduction wins.
    #[test]
    fn an_icc_profile_disagreeing_with_n_falls_back() {
        let doc = icc_doc("/N 4", &rgb_profile(&srgb_trc()));
        let cs = ColorSpace::parse(&doc, &obj(b"[/ICCBased 4 0 R]"));
        assert_eq!(cs, ColorSpace::DeviceCMYK);
    }

    /// CalGray with gamma 2,2 renders mid-gray as the sRGB encoding of
    /// 0,5^2,2; CalRGB with default gamma and matrix sends white to white;
    /// Lab maps L* = 100 to white and +a* toward red.
    #[test]
    fn cie_spaces_convert() {
        let doc = test_doc();
        let p = |s: &[u8]| ColorSpace::parse(&doc, &obj(s));

        let gray = p(b"[/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 2.2 >>]");
        assert_eq!(gray.components(), 1);
        let out = gray.to_rgb(&[0.5]);
        let want = srgb_encode(0.5f32.powf(2.2));
        assert!((out[0] - want).abs() < 1e-4, "{out:?} want {want}");
        assert_eq!(out[0], out[1]);

        // With the default identity matrix, device white decodes to
        // XYZ (1, 1, 1); declaring that as the whitepoint makes the
        // adaptation send it exactly to sRGB white.
        let rgb = p(b"[/CalRGB << /WhitePoint [1 1 1] >>]");
        let white = rgb.to_rgb(&[1.0, 1.0, 1.0]);
        for c in white {
            assert!((c - 1.0).abs() < 1e-3, "{white:?}");
        }
        let red = rgb.to_rgb(&[1.0, 0.0, 0.0]);
        assert!(red[0] > red[1] && red[0] > red[2], "{red:?}");

        let lab = p(b"[/Lab << /WhitePoint [0.9505 1 1.089] >>]");
        assert_eq!(lab.components(), 3);
        let white = lab.to_rgb(&[100.0, 0.0, 0.0]);
        for c in white {
            assert!((c - 1.0).abs() < 1e-3, "{white:?}");
        }
        let red = lab.to_rgb(&[50.0, 60.0, 0.0]);
        assert!(red[0] > red[1], "{red:?}");
        let black = lab.to_rgb(&[0.0, 0.0, 0.0]);
        assert!(black.iter().all(|&v| v < 1e-3), "{black:?}");
    }

    /// An `/Indexed` palette over a Lab base rescales its bytes into the
    /// Lab component ranges instead of 0..=1.
    #[test]
    fn indexed_over_lab_expands_palette() {
        let doc = test_doc();
        let cs = ColorSpace::parse(
            &doc,
            &obj(b"[/Indexed [/Lab << /WhitePoint [0.9505 1 1.089] >>] 1 <FF8080 008080>]"),
        );
        let white = cs.to_rgb(&[0.0]);
        for c in white {
            assert!((c - 1.0).abs() < 5e-3, "{white:?}");
        }
        // The palette byte 128 lands at a* = b* = 0,392 rather than 0, so
        // the black entry keeps a sub-percent red cast.
        let black = cs.to_rgb(&[1.0]);
        assert!(black.iter().all(|&v| v < 0.01), "{black:?}");
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
