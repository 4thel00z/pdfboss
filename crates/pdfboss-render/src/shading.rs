//! Shading dictionaries (ISO 32000-1 §8.7.4.5): axial (type 2) and radial
//! (type 3) shadings evaluated through function types 0 (sampled), 2
//! (exponential) and 3 (stitching), painted per device pixel under a
//! coverage mask. Function-based (type 1) and mesh (types 4-7) shadings and
//! PostScript calculator functions (type 4) load as `None` so the caller
//! reports them as unsupported instead of guessing.

use pdfboss_core::geom::{Matrix, Point};
use pdfboss_core::{decoded_stream_data_with, AsyncObjectSource, Dict, Error, Object};

use crate::color::ColorSpace;
use crate::raster::{composite_over, Mask};
use crate::Pixmap;

/// Most components any color space here carries (CMYK is 4; `Other` caps at
/// what `ColorSpace::to_rgb` reads).
pub(crate) const MAX_COMPS: usize = 8;

/// Upper bound on parsed function nodes per shading. A real gradient uses a
/// handful (one stitching function over a few exponentials); the cap only
/// stops a hostile file minting nodes without limit.
const MAX_FUNCTIONS: usize = 256;

/// One parsed function. Stitching children are arena indices, so loading
/// needs no recursion (a queue fills the arena) and evaluation recurses
/// over indices with the depth bounded by [`MAX_FUNCTIONS`].
#[derive(Debug, Clone, PartialEq)]
enum Node {
    /// Type 2: `C0 + x^N (C1 - C0)` over `domain`.
    Exponential {
        domain: [f32; 2],
        c0: Vec<f32>,
        c1: Vec<f32>,
        n: f32,
    },
    /// Type 3: subfunction `i` covers `[bounds[i-1], bounds[i])`, its input
    /// re-mapped through `encode[2i..2i+2]`.
    Stitching {
        domain: [f32; 2],
        children: Vec<usize>,
        bounds: Vec<f32>,
        encode: Vec<f32>,
    },
    /// Type 0: `outputs` values per sample, `bps` bits each, big-endian,
    /// linearly interpolated between the two nearest of `size` samples.
    Sampled {
        domain: [f32; 2],
        encode: [f32; 2],
        decode: Vec<f32>,
        size: usize,
        bps: u32,
        outputs: usize,
        data: Vec<u8>,
    },
}

/// The functions a shading evaluates: one n-output function, or an array of
/// single-output functions whose results concatenate (§8.7.4.5.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Functions {
    nodes: Vec<Node>,
    roots: Vec<usize>,
}

impl Functions {
    /// Evaluates every root at `x` into `out`, returning how many
    /// components were written.
    pub(crate) fn eval(&self, x: f32, out: &mut [f32; MAX_COMPS]) -> usize {
        let mut written = 0;
        for &root in &self.roots {
            if written >= MAX_COMPS {
                break;
            }
            written += self.eval_node(root, x, &mut out[written..]);
        }
        written
    }

    fn eval_node(&self, idx: usize, x: f32, out: &mut [f32]) -> usize {
        match &self.nodes[idx] {
            Node::Exponential { domain, c0, c1, n } => {
                let x = x.clamp(domain[0], domain[1]);
                // x^1 is the overwhelmingly common gradient; skip the powf.
                let xn = if *n == 1.0 { x } else { x.powf(*n) };
                let count = c0.len().min(out.len());
                for (i, slot) in out.iter_mut().enumerate().take(count) {
                    *slot = c0[i] + xn * (c1[i] - c0[i]);
                }
                count
            }
            Node::Stitching {
                domain,
                children,
                bounds,
                encode,
            } => {
                let x = x.clamp(domain[0], domain[1]);
                // Subinterval k: bounds partition [domain0, domain1).
                let k = bounds.iter().take_while(|&&b| x >= b).count();
                let Some(&child) = children.get(k) else {
                    return 0;
                };
                let lo = if k == 0 { domain[0] } else { bounds[k - 1] };
                let hi = bounds.get(k).copied().unwrap_or(domain[1]);
                let (e0, e1) = (encode[2 * k], encode[2 * k + 1]);
                let t = if hi > lo { (x - lo) / (hi - lo) } else { 0.0 };
                self.eval_node(child, e0 + t * (e1 - e0), out)
            }
            Node::Sampled {
                domain,
                encode,
                decode,
                size,
                bps,
                outputs,
                data,
            } => {
                let x = x.clamp(domain[0], domain[1]);
                let span = domain[1] - domain[0];
                let t = if span > 0.0 {
                    (x - domain[0]) / span
                } else {
                    0.0
                };
                let e = (encode[0] + t * (encode[1] - encode[0])).clamp(0.0, (*size - 1) as f32);
                let i0 = e.floor() as usize;
                let i1 = (i0 + 1).min(*size - 1);
                let frac = e - i0 as f32;
                let count = (*outputs).min(out.len()).min(decode.len() / 2);
                let max = ((1u64 << *bps) - 1) as f32;
                for (j, slot) in out.iter_mut().enumerate().take(count) {
                    let s0 = sample_at(data, (i0 * outputs + j) as u64, *bps) as f32 / max;
                    let s1 = sample_at(data, (i1 * outputs + j) as u64, *bps) as f32 / max;
                    let s = s0 + (s1 - s0) * frac;
                    *slot = decode[2 * j] + s * (decode[2 * j + 1] - decode[2 * j]);
                }
                count
            }
        }
    }
}

/// Reads big-endian sample `index` of `bps` bits from a packed bit stream,
/// 0 when the data ends early (the truncated region reads as zero samples,
/// the same leniency images get).
fn sample_at(data: &[u8], index: u64, bps: u32) -> u64 {
    let mut value = 0u64;
    let start = index * bps as u64;
    for bit in start..start + bps as u64 {
        let byte = (bit / 8) as usize;
        let within = 7 - (bit % 8) as u32;
        let b = data.get(byte).copied().unwrap_or(0);
        value = (value << 1) | u64::from((b >> within) & 1);
    }
    value
}

/// The geometry of a supported shading.
enum Geometry {
    /// Type 2: the axis from `p0` to `p1`.
    Axial { p0: Point, p1: Point },
    /// Type 3: circles blended from `(c0, r0)` to `(c1, r1)`.
    Radial {
        c0: Point,
        r0: f32,
        c1: Point,
        r1: f32,
    },
}

impl Geometry {
    /// The shading parameter `s` (0..=1) painting point `p`, or `None` when
    /// `p` lies outside the shading and its extensions. Extension paints the
    /// endpoint color, so out-of-range values clamp when the matching
    /// `extend` flag allows them.
    fn param_at(&self, p: Point, extend: [bool; 2]) -> Option<f32> {
        match self {
            Geometry::Axial { p0, p1 } => {
                let dx = p1.x - p0.x;
                let dy = p1.y - p0.y;
                let denom = dx * dx + dy * dy;
                if !denom.is_finite() || denom <= 0.0 {
                    return None;
                }
                let s = ((p.x - p0.x) * dx + (p.y - p0.y) * dy) / denom;
                clamp_extended(s, extend)
            }
            Geometry::Radial { c0, r0, c1, r1 } => {
                let dcx = c1.x - c0.x;
                let dcy = c1.y - c0.y;
                let dr = r1 - r0;
                let qx = p.x - c0.x;
                let qy = p.y - c0.y;
                // |p - c(s)| = r(s) as a quadratic in s (§8.7.4.5.4).
                let a = dcx * dcx + dcy * dcy - dr * dr;
                let b = -2.0 * (qx * dcx + qy * dcy + r0 * dr);
                let c = qx * qx + qy * qy - r0 * r0;
                let (lo, hi) = (
                    if extend[0] { f32::NEG_INFINITY } else { 0.0 },
                    if extend[1] { f32::INFINITY } else { 1.0 },
                );
                let mut best: Option<f32> = None;
                let mut consider = |s: f32| {
                    if !s.is_finite() || s < lo || s > hi || r0 + s * dr < 0.0 {
                        return;
                    }
                    // The largest s wins: later circles paint over earlier.
                    if best.is_none_or(|b| s > b) {
                        best = Some(s);
                    }
                };
                if a.abs() > 1e-6 {
                    let disc = b * b - 4.0 * a * c;
                    if disc < 0.0 {
                        return None;
                    }
                    let sq = disc.sqrt();
                    consider((-b + sq) / (2.0 * a));
                    consider((-b - sq) / (2.0 * a));
                } else if b.abs() > 1e-6 {
                    consider(-c / b);
                } else {
                    return None;
                }
                best.and_then(|s| clamp_extended(s, extend))
            }
        }
    }
}

/// Clamps an extended parameter to 0..=1, or rejects it where the matching
/// `/Extend` flag is off.
fn clamp_extended(s: f32, extend: [bool; 2]) -> Option<f32> {
    if !s.is_finite() {
        return None;
    }
    if s < 0.0 {
        return extend[0].then_some(0.0);
    }
    if s > 1.0 {
        return extend[1].then_some(1.0);
    }
    Some(s)
}

/// A loaded, paintable shading.
pub(crate) struct Shading {
    geometry: Geometry,
    cs: ColorSpace,
    functions: Functions,
    domain: [f32; 2],
    extend: [bool; 2],
    /// Optional clip in the shading's own target space (`/BBox`); the caller
    /// intersects it into the paint region under the same matrix the shading
    /// paints with.
    pub(crate) bbox: Option<[f32; 4]>,
    /// `/Background`, resolved to RGB — painted behind a shading *pattern*
    /// fill (never behind `sh`, §8.7.4.3).
    pub(crate) background: Option<[f32; 3]>,
}

/// Reads the first `n` finite numbers of a (possibly indirect) array.
async fn floats<S: AsyncObjectSource>(src: &S, obj: Option<&Object>, n: usize) -> Option<Vec<f32>> {
    let arr = match src.resolve(obj?).await {
        Ok(Object::Array(a)) if a.len() >= n => a,
        _ => return None,
    };
    let mut out = Vec::with_capacity(n);
    for o in arr.iter().take(n) {
        match src.resolve(o).await {
            Ok(v) => match v.as_f64() {
                Some(f) if (f as f32).is_finite() => out.push(f as f32),
                _ => return None,
            },
            Err(_) => return None,
        }
    }
    Some(out)
}

/// Reads a whole numeric array of any length.
async fn float_array<S: AsyncObjectSource>(src: &S, obj: Option<&Object>) -> Option<Vec<f32>> {
    let arr = match src.resolve(obj?).await {
        Ok(Object::Array(a)) => a,
        _ => return None,
    };
    let n = arr.len();
    floats(src, Some(&Object::Array(arr)), n).await
}

/// The dictionary of a function object, which is a plain dictionary for
/// types 2 and 3 and a stream for types 0 and 4.
async fn function_dict<S: AsyncObjectSource>(
    src: &S,
    obj: &Object,
) -> Option<(Dict, Option<Vec<u8>>)> {
    match src.resolve(obj).await.ok()? {
        Object::Dict(d) => Some((d, None)),
        Object::Stream(s) => {
            let data = decoded_stream_data_with(src, &s).await.ok()?;
            Some((s.dict.clone(), Some(data)))
        }
        _ => None,
    }
}

/// Loads `/Function` — one function or an array of them — into an arena.
/// `Ok(None)` means a function *type* nobody evaluates here (the PostScript
/// calculator); `Err` is a structural failure worth reporting verbatim.
pub(crate) async fn load_functions<S: AsyncObjectSource>(
    src: &S,
    obj: &Object,
) -> Result<Option<Functions>, Error> {
    let mut queue: Vec<Object> = Vec::new();
    match src.resolve(obj).await {
        Ok(Object::Array(items)) => queue.extend(items.iter().cloned()),
        Ok(other) => queue.push(other),
        Err(e) => return Err(e),
    }
    let mut functions = Functions {
        nodes: Vec::new(),
        roots: Vec::new(),
    };
    // The queue holds (object, destination): a root, or a stitching child
    // slot that must be patched once the node index exists.
    let mut work: Vec<(Object, Option<(usize, usize)>)> =
        queue.into_iter().map(|o| (o, None)).collect();
    let mut cursor = 0;
    while cursor < work.len() {
        if functions.nodes.len() >= MAX_FUNCTIONS {
            return Err(Error::Other("shading function tree too large".into()));
        }
        let (obj, parent) = work[cursor].clone();
        cursor += 1;
        let Some((dict, data)) = function_dict(src, &obj).await else {
            return Err(Error::Other("shading function is not one".into()));
        };
        let kind = dict.get_int("FunctionType").unwrap_or(-1);
        let domain: [f32; 2] = match floats(src, dict.get("Domain"), 2).await {
            Some(d) => [d[0], d[1]],
            None => [0.0, 1.0],
        };
        let node = match kind {
            2 => {
                let c0 = float_array(src, dict.get("C0")).await.unwrap_or(vec![0.0]);
                let c1 = float_array(src, dict.get("C1")).await.unwrap_or(vec![1.0]);
                if c0.len() != c1.len() || c0.is_empty() {
                    return Err(Error::Other("exponential function C0/C1 disagree".into()));
                }
                let n = floats(src, dict.get("N"), 1)
                    .await
                    .map(|v| v[0])
                    .or_else(|| dict.get_int("N").map(|n| n as f32))
                    .unwrap_or(1.0);
                Node::Exponential { domain, c0, c1, n }
            }
            3 => {
                let subs = match src
                    .resolve(dict.get("Functions").ok_or_else(|| {
                        Error::Other("stitching function has no /Functions".into())
                    })?)
                    .await
                {
                    Ok(Object::Array(a)) => a,
                    _ => return Err(Error::Other("stitching /Functions is not an array".into())),
                };
                if subs.is_empty() {
                    return Err(Error::Other("stitching function has no parts".into()));
                }
                let bounds = float_array(src, dict.get("Bounds"))
                    .await
                    .unwrap_or_default();
                let encode = float_array(src, dict.get("Encode"))
                    .await
                    .unwrap_or_else(|| (0..subs.len()).flat_map(|_| [0.0, 1.0]).collect());
                if bounds.len() + 1 != subs.len() || encode.len() < 2 * subs.len() {
                    return Err(Error::Other("stitching bounds do not partition".into()));
                }
                let mut children = Vec::with_capacity(subs.len());
                let node_index = functions.nodes.len();
                for (slot, sub) in subs.iter().enumerate() {
                    children.push(usize::MAX); // patched when the child loads
                    work.push((sub.clone(), Some((node_index, slot))));
                }
                Node::Stitching {
                    domain,
                    children,
                    bounds,
                    encode,
                }
            }
            0 => {
                let data =
                    data.ok_or_else(|| Error::Other("sampled function carries no stream".into()))?;
                let size = match float_array(src, dict.get("Size")).await {
                    // Shading functions take one input; a multi-input
                    // sampled function has no meaning here.
                    Some(s) if s.len() == 1 && s[0] >= 1.0 => s[0] as usize,
                    _ => return Err(Error::Other("sampled function /Size unusable".into())),
                };
                let bps = match dict.get_int("BitsPerSample") {
                    Some(b @ (1 | 2 | 4 | 8 | 12 | 16 | 24 | 32)) => b as u32,
                    _ => return Err(Error::Other("sampled function bits unusable".into())),
                };
                let range = float_array(src, dict.get("Range"))
                    .await
                    .filter(|r| r.len() >= 2 && r.len() % 2 == 0)
                    .ok_or_else(|| Error::Other("sampled function has no /Range".into()))?;
                let outputs = range.len() / 2;
                let encode = match floats(src, dict.get("Encode"), 2).await {
                    Some(e) => [e[0], e[1]],
                    None => [0.0, (size - 1) as f32],
                };
                let decode = float_array(src, dict.get("Decode"))
                    .await
                    .filter(|d| d.len() == range.len())
                    .unwrap_or(range);
                Node::Sampled {
                    domain,
                    encode,
                    decode,
                    size,
                    bps,
                    outputs,
                    data,
                }
            }
            4 => return Ok(None),
            _ => return Err(Error::Other("unknown function type".into())),
        };
        let index = functions.nodes.len();
        functions.nodes.push(node);
        match parent {
            None => functions.roots.push(index),
            Some((parent_index, slot)) => {
                if let Node::Stitching { children, .. } = &mut functions.nodes[parent_index] {
                    children[slot] = index;
                }
            }
        }
    }
    // A stitching child that never loaded leaves usize::MAX behind; the cap
    // above is the only way to get here, and it already errored.
    Ok(Some(functions))
}

impl Shading {
    /// Loads a shading dictionary (or stream — mesh shadings are streams,
    /// and they answer `Ok(None)`). `Ok(None)` = a shading or function type
    /// this renderer does not paint, for the caller to report as
    /// unsupported; `Err` = a structural failure, reported verbatim.
    pub(crate) async fn load_with<S: AsyncObjectSource>(
        src: &S,
        obj: &Object,
    ) -> Result<Option<Shading>, Error> {
        let dict = match src.resolve(obj).await? {
            Object::Dict(d) => d,
            Object::Stream(s) => s.dict.clone(),
            _ => return Err(Error::Other("shading is not a dictionary".into())),
        };
        let kind = dict.get_int("ShadingType").unwrap_or(-1);
        let geometry = match kind {
            2 => {
                let c = floats(src, dict.get("Coords"), 4)
                    .await
                    .ok_or_else(|| Error::Other("axial shading /Coords unusable".into()))?;
                Geometry::Axial {
                    p0: Point { x: c[0], y: c[1] },
                    p1: Point { x: c[2], y: c[3] },
                }
            }
            3 => {
                let c = floats(src, dict.get("Coords"), 6)
                    .await
                    .ok_or_else(|| Error::Other("radial shading /Coords unusable".into()))?;
                if c[2] < 0.0 || c[5] < 0.0 {
                    return Err(Error::Other("radial shading radius negative".into()));
                }
                Geometry::Radial {
                    c0: Point { x: c[0], y: c[1] },
                    r0: c[2],
                    c1: Point { x: c[3], y: c[4] },
                    r1: c[5],
                }
            }
            1 | 4..=7 => return Ok(None),
            _ => return Err(Error::Other("unknown shading type".into())),
        };
        let cs_obj = dict
            .get("ColorSpace")
            .ok_or_else(|| Error::Other("shading has no /ColorSpace".into()))?;
        let cs = ColorSpace::parse_with(src, cs_obj).await;
        let functions = match dict.get("Function") {
            Some(f) => match load_functions(src, f).await? {
                Some(functions) => functions,
                None => return Ok(None),
            },
            None => return Err(Error::Other("shading has no /Function".into())),
        };
        let domain = match floats(src, dict.get("Domain"), 2).await {
            Some(d) => [d[0], d[1]],
            None => [0.0, 1.0],
        };
        let extend = match src
            .resolve(dict.get("Extend").unwrap_or(&Object::Null))
            .await
        {
            Ok(Object::Array(a)) if a.len() >= 2 => [
                matches!(a[0], Object::Bool(true)),
                matches!(a[1], Object::Bool(true)),
            ],
            _ => [false, false],
        };
        let bbox = floats(src, dict.get("BBox"), 4)
            .await
            .map(|b| [b[0], b[1], b[2], b[3]]);
        let background = float_array(src, dict.get("Background"))
            .await
            .map(|comps| cs.to_rgb(&comps));
        Ok(Some(Shading {
            geometry,
            cs,
            functions,
            domain,
            extend,
            bbox,
            background,
        }))
    }

    /// Paints the shading over every pixel `region` covers (the whole page
    /// when `None`), compositing at `alpha` × coverage. `to_device` maps the
    /// shading's target space to device pixels; a singular matrix paints
    /// nothing (the caller reports it).
    pub(crate) fn paint(
        &self,
        pix: &mut Pixmap,
        region: Option<&Mask>,
        alpha: f32,
        to_device: Matrix,
    ) {
        let Some(inv) = to_device.invert() else {
            return;
        };
        let alpha = if alpha.is_finite() {
            alpha.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if alpha <= 0.0 {
            return;
        }
        let (x_lo, x_hi, y_lo, y_hi) = match region {
            Some(m) => (
                m.x0,
                (m.x0 + m.bbox_w).min(pix.width),
                m.y0,
                (m.y0 + m.bbox_h).min(pix.height),
            ),
            None => (0, pix.width, 0, pix.height),
        };
        let mut comps = [0f32; MAX_COMPS];
        for y in y_lo..y_hi {
            for x in x_lo..x_hi {
                let cov = region.map_or(255, |m| m.coverage(x, y));
                if cov == 0 {
                    continue;
                }
                let p = inv.apply(Point {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                });
                let Some(s) = self.geometry.param_at(p, self.extend) else {
                    continue;
                };
                let t = self.domain[0] + (self.domain[1] - self.domain[0]) * s;
                let n = self.functions.eval(t, &mut comps);
                let rgb = self.cs.to_rgb(&comps[..n]);
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                let rgb8 = [q(rgb[0]), q(rgb[1]), q(rgb[2])];
                let a = alpha * cov as f32 / 255.0;
                let off = ((y * pix.width + x) * 4) as usize;
                if a >= 1.0 {
                    let opaque = [rgb8[0], rgb8[1], rgb8[2], 255];
                    pix.data[off..off + 4].copy_from_slice(&opaque);
                } else {
                    composite_over(&mut pix.data[off..off + 4], rgb8, a);
                }
            }
        }
    }
}
