//! Component and colour handling (ITU-T T.800 Annex G + Annex I): inverse
//! multiple component transformation, DC level shift, palette application,
//! sYCC conversion, sample normalization, and compositing decoded tiles
//! into the final interleaved image — the dwt → color seam.

use crate::boxes::{ChannelDefinition, ColorSpec, Jp2Header, Palette};
use crate::dequant::{CoefficientCanvas, TileComponentCanvas};
use crate::error::{JpxError, Result};
use crate::geometry::Rect;
use crate::markers::{Siz, SizComponent, WaveletKind};
use crate::{ColorKind, DecodeLimits, DecodedImage, JpxWarning};

/// Inverse RCT per T.800 G.2.2, Equations (G-6)..(G-8). Integer exact:
/// `div_euclid(4)` IS the floor division the corner brackets denote (the
/// divisor is positive). Returns `(I0, I1, I2)`.
fn inverse_rct(y0: i64, y1: i64, y2: i64) -> (i64, i64, i64) {
    // (G-6): I1 = Y0 - floor((Y2 + Y1) / 4).
    let i1 = y0 - (y2 + y1).div_euclid(4);
    // (G-7): I0 = Y2 + I1;  (G-8): I2 = Y1 + I1.
    (y2 + i1, i1, y1 + i1)
}

/// Inverse RCT on non-integer canvases: the (G-6)..(G-8) formulas with
/// `f64::floor` standing in for the corner brackets. Needed when Table
/// A.17 pairs the RCT with the 5-3 filter but signalled quantization put
/// the coefficients on the f64 path.
fn inverse_rct_f64(y0: f64, y1: f64, y2: f64) -> (f64, f64, f64) {
    // (G-6): I1 = Y0 - floor((Y2 + Y1) / 4).
    let i1 = y0 - ((y2 + y1) / 4.0).floor();
    // (G-7): I0 = Y2 + I1;  (G-8): I2 = Y1 + I1.
    (y2 + i1, i1, y1 + i1)
}

/// Inverse ICT per T.800 G.3.2, Equations (G-12)..(G-14) — G.3.2 states
/// the coefficients imply no required precision, so f64 is used
/// throughout. Returns `(I0, I1, I2)`.
fn inverse_ict(y0: f64, y1: f64, y2: f64) -> (f64, f64, f64) {
    (
        y0 + 1.402 * y2,                  // (G-12)
        y0 - 0.34413 * y1 - 0.71414 * y2, // (G-13)
        y0 + 1.772 * y1,                  // (G-14)
    )
}

/// Clamps a bit depth to the 1..=38 range both Table A.11 (Ssiz) and
/// Table I.13 (palette B values) allow, so no shift below can overflow an
/// i64 on hostile header bytes.
fn usable_depth(depth: u8) -> u32 {
    u32::from(depth).clamp(1, 38)
}

/// Inverse DC level shift (G-2, unsigned only) + range clamp (G.1.2 NOTE).
///
/// Unsigned samples gain `2^(Ssiz)` where Ssiz is the marker byte value,
/// i.e. `depth - 1` (Table A.11 stores depth - 1; the parser applied the
/// `+ 1`), then clip to `0..=2^depth - 1`. Signed samples are not shifted
/// (G.1.2 applies to unsigned components only), only clipped to
/// `-2^(depth-1)..=2^(depth-1) - 1` — the G.1.2 NOTE's "typical solution"
/// for out-of-range reconstructions.
fn level_shift_and_clamp(value: i64, depth: u8, signed: bool) -> i64 {
    let d = usable_depth(depth);
    let half = 1i64 << (d - 1);
    if signed {
        value.clamp(-half, half - 1)
    } else {
        (value + half).clamp(0, (1i64 << d) - 1)
    }
}

/// Normalizes one clamped component/palette sample to the 8-bit output.
///
/// Signed samples are first re-centred to unsigned by adding `2^(depth-1)`
/// (the G.1.2 shift, applied at output time because the codestream carried
/// them signed). Then, per the crate contract: depth > 8 right-shifts by
/// `depth - 8` with round-to-nearest — `(v + 2^(shift-1)) >> shift`,
/// saturated at 255 (the midpoint `2^(depth-1)` still lands exactly on
/// 128, which the sYCC chroma centring relies on); depth < 8 scales to
/// the full range as `round(v * 255 / (2^depth - 1))` (computed as
/// `(v*255 + floor(max/2)) / max`); depth 8 passes through.
fn normalize_to_u8(value: i64, depth: u8, signed: bool) -> u8 {
    let d = usable_depth(depth);
    let half = 1i64 << (d - 1);
    let max = (1i64 << d) - 1;
    let v = if signed { value + half } else { value }.clamp(0, max);
    if d > 8 {
        let shift = d - 8;
        ((v + (1i64 << (shift - 1))) >> shift).min(255) as u8
    } else if d < 8 {
        ((v * 255 + max / 2) / max) as u8
    } else {
        v as u8
    }
}

/// Rounds and clips an f64 into one 8-bit output sample (sYCC path).
fn clamp_round_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// How one output channel sources its samples (I.5.3.5 MTYP semantics).
#[derive(Clone, Copy, Debug)]
enum ChannelSource {
    /// MTYP 0, or the identity mapping when no cmap box exists (I.5.3.5):
    /// the codestream component is used directly.
    Direct {
        /// Codestream component index.
        component: usize,
    },
    /// MTYP 1: the component's samples index one palette column (I.5.3.4).
    Palette {
        /// Codestream component index (the palette index source).
        component: usize,
        /// Palette column (PCOL).
        column: usize,
    },
    /// An inconsistent cmap entry neutralized to zero fill (fail-soft; the
    /// inconsistency was reported as a warning when the channel was built).
    Zero,
}

/// One output channel: its source plus the bit depth/signedness that drive
/// the 8-bit normalization (the component's Ssiz for direct use, the
/// palette column's B value for palette mapping).
#[derive(Clone, Copy, Debug)]
struct Channel {
    source: ChannelSource,
    depth: u8,
    signed: bool,
}

/// Identity mapping: component i -> channel i (I.5.3.5, mandated when no
/// cmap box is present; also the raw-codestream layout).
fn identity_channels(components: &[SizComponent]) -> Vec<Channel> {
    components
        .iter()
        .enumerate()
        .map(|(component, spec)| Channel {
            source: ChannelSource::Direct { component },
            depth: spec.depth,
            signed: spec.signed,
        })
        .collect()
}

/// Builds the output channel list from the header's cmap/pclr pair
/// (I.5.3.4/I.5.3.5), falling back to the identity mapping. Inconsistent
/// entries degrade per-channel with a warning instead of failing the
/// decode (the codestream itself is fine; only JP2 metadata is broken).
fn build_channels(
    siz: &Siz,
    header: Option<&Jp2Header>,
    warnings: &mut Vec<JpxWarning>,
) -> (Vec<Channel>, Option<Palette>) {
    let Some(header) = header else {
        return (identity_channels(&siz.components), None);
    };
    if header.component_mapping.is_empty() {
        if header.palette.is_some() {
            warnings.push(JpxWarning::note(
                "pclr without a cmap box ignored (I.5.3.4 requires both)",
            ));
        }
        return (identity_channels(&siz.components), None);
    }
    // A palette is usable only when its Table I.12 invariants hold.
    let palette = header.palette.as_ref().and_then(|palette| {
        let columns = usize::from(palette.created_channels);
        let consistent = (1..=1024).contains(&palette.entries)
            && columns >= 1
            && palette.channel_depths.len() == columns
            && palette.values.len() == usize::from(palette.entries) * columns;
        if consistent {
            Some(palette.clone())
        } else {
            warnings.push(JpxWarning::note(
                "pclr box internally inconsistent (Table I.12); palette ignored",
            ));
            None
        }
    });
    let zero_channel = Channel {
        source: ChannelSource::Zero,
        depth: 8,
        signed: false,
    };
    let mut channels = Vec::with_capacity(header.component_mapping.len());
    for (index, mapping) in header.component_mapping.iter().enumerate() {
        let component = usize::from(mapping.component);
        let Some(spec) = siz.components.get(component) else {
            // A zero-filled channel is missing pixels.
            warnings.push(JpxWarning::loss(format!(
                "cmap channel {index}: codestream component {component} does not exist; \
                 channel zero-filled"
            )));
            channels.push(zero_channel);
            continue;
        };
        let direct = Channel {
            source: ChannelSource::Direct { component },
            depth: spec.depth,
            signed: spec.signed,
        };
        match (mapping.mapping_type, &palette) {
            (0, _) => channels.push(direct),
            (1, Some(palette)) => {
                let column = usize::from(mapping.palette_column);
                match palette.channel_depths.get(column) {
                    Some(&raw) => channels.push(Channel {
                        source: ChannelSource::Palette { component, column },
                        // Table I.13: low 7 bits store depth - 1, the MSB
                        // the sign flag.
                        depth: (raw & 127) + 1,
                        signed: raw & 128 != 0,
                    }),
                    None => {
                        // A zero-filled channel is missing pixels.
                        warnings.push(JpxWarning::loss(format!(
                            "cmap channel {index}: palette column {column} out of range; \
                             channel zero-filled"
                        )));
                        channels.push(zero_channel);
                    }
                }
            }
            (1, None) => {
                // Palette indices shown as intensities are WRONG pixels:
                // the intended colours are unknowable without the palette.
                warnings.push(JpxWarning::loss(format!(
                    "cmap channel {index}: palette mapping without a usable pclr box; \
                     component {component} used directly"
                )));
                channels.push(direct);
            }
            (other, _) => {
                warnings.push(JpxWarning::note(format!(
                    "cmap channel {index}: reserved MTYP {other} (Table I.14); \
                     component {component} used directly"
                )));
                channels.push(direct);
            }
        }
    }
    (channels, palette)
}

/// Looks one level-shifted component sample up in the palette. Hostile
/// indices clamp to the table (I.5.3.4 defines no out-of-range behaviour;
/// clamping is the fail-soft choice), and the flat access stays
/// bounds-checked besides.
fn palette_value(palette: &Palette, index: i64, column: usize) -> i64 {
    let last = i64::from(palette.entries).max(1) - 1;
    let entry = index.clamp(0, last) as usize;
    palette
        .values
        .get(entry * usize::from(palette.created_channels) + column)
        .copied()
        .map(i64::from)
        .unwrap_or(0)
}

/// One tile-component canvas widened for transform arithmetic: i64 for the
/// reversible path (integer exactness), f64 for the irreversible one.
enum WorkCanvas {
    Int(Vec<i64>),
    Float(Vec<f64>),
}

/// Undoes the Table A.17 multiple component transformation in place on
/// the first three tile-components. WHICH transform is decided by the
/// filter those components signalled — Table A.17 pairs the RCT with the
/// 5-3 wavelet and the ICT with the 9-7 — never by the canvas arithmetic:
/// a 5-3 tile with signalled quantization runs on f64 canvases yet still
/// takes the RCT (via its floor-division f64 form). `wavelet` is the
/// common filter of components 0..3, `None` when they disagree — an
/// illegal pairing under G.2.1/G.3.1, skipped with a warning. G.2/G.3
/// also demand identical separations, so mismatched rects skip too.
fn apply_inverse_mct(
    work: &mut [(Rect, WorkCanvas)],
    wavelet: Option<WaveletKind>,
    warnings: &mut Vec<JpxWarning>,
) {
    let [first, second, third, ..] = work else {
        // A skipped component transform leaves wrong colours behind.
        warnings.push(JpxWarning::loss(
            "MCT signalled with fewer than three components; transform skipped (G.2/G.3)",
        ));
        return;
    };
    if first.0 != second.0 || first.0 != third.0 {
        // A skipped component transform leaves wrong colours behind.
        warnings.push(JpxWarning::loss(
            "MCT components disagree on their tile-component rects \
             (G.2/G.3 demand identical separations); transform skipped",
        ));
        return;
    }
    let Some(wavelet) = wavelet else {
        // A skipped component transform leaves wrong colours behind.
        warnings.push(JpxWarning::loss(
            "MCT components disagree on their wavelet filter, so Table A.17 \
             pairs no transform with them (G.2.1/G.3.1); transform skipped",
        ));
        return;
    };
    match (wavelet, &mut first.1, &mut second.1, &mut third.1) {
        (WaveletKind::Reversible53, WorkCanvas::Int(a), WorkCanvas::Int(b), WorkCanvas::Int(c)) => {
            // 5-3 reversible path: inverse RCT (G-6)..(G-8).
            for ((y0, y1), y2) in a.iter_mut().zip(b.iter_mut()).zip(c.iter_mut()) {
                let (i0, i1, i2) = inverse_rct(*y0, *y1, *y2);
                *y0 = i0;
                *y1 = i1;
                *y2 = i2;
            }
        }
        (
            WaveletKind::Reversible53,
            WorkCanvas::Float(a),
            WorkCanvas::Float(b),
            WorkCanvas::Float(c),
        ) => {
            // Still the RCT (Table A.17 keys on the FILTER): signalled
            // quantization put the 5-3 coefficients on the f64 path.
            for ((y0, y1), y2) in a.iter_mut().zip(b.iter_mut()).zip(c.iter_mut()) {
                let (i0, i1, i2) = inverse_rct_f64(*y0, *y1, *y2);
                *y0 = i0;
                *y1 = i1;
                *y2 = i2;
            }
        }
        (
            WaveletKind::Irreversible97,
            WorkCanvas::Float(a),
            WorkCanvas::Float(b),
            WorkCanvas::Float(c),
        ) => {
            // 9-7 irreversible path: inverse ICT (G-12)..(G-14).
            for ((y0, y1), y2) in a.iter_mut().zip(b.iter_mut()).zip(c.iter_mut()) {
                let (i0, i1, i2) = inverse_ict(*y0, *y1, *y2);
                *y0 = i0;
                *y1 = i1;
                *y2 = i2;
            }
        }
        // Mixed canvas kinds under one filter (per-component quantization
        // styles differ), or an integer canvas claiming the 9-7 (cannot
        // arise: the integer path requires the 5-3). A skipped component
        // transform leaves wrong colours behind.
        _ => warnings.push(JpxWarning::loss(
            "MCT components mix reversible and irreversible canvases; transform skipped \
             (Table A.17 pairs the RCT with the 5-3 filter and the ICT with the 9-7)",
        )),
    }
}

/// Accumulates decoded tiles into the final image.
///
/// Responsibilities, in application order per tile: inverse RCT (G.2.2,
/// integer-exact, 5-3 path) or inverse ICT (G.3.2, f32, 9-7 path) when the
/// tile's MCT flag is set (Table A.17); inverse DC level shift (G.1.2, and
/// Table A.11 signedness); palette + component mapping (I.5.3.4/I.5.3.5);
/// sYCC → RGB when colr signals EnumCS 18 (I.5.3.3); replication upsampling
/// of subsampled components onto the reference grid (G.4/B.2); deep
/// depths right-shifted to 8 with round-to-nearest and everything clamped
/// to 0..=255 (crate contract). `finish` crops to the image region — the canvas starts at
/// (XOsiz, YOsiz), size (Xsiz - XOsiz) x (Ysiz - YOsiz) (B-1/B-2) — and
/// reports the cdef opacity channel (I.5.3.6) as `alpha_index`.
// Internal state is the colour stage's to design; only the three method
// signatures below are the frozen seam.
pub(crate) struct ImageAssembler {
    /// The image region on the reference grid (B-1): [XOsiz, Xsiz) x
    /// [YOsiz, Ysiz). The output buffer covers exactly this rect.
    region: Rect,
    /// Per-codestream-component SIZ parameters (depth, signedness, and the
    /// XRsiz/YRsiz separations that drive replication upsampling).
    components: Vec<SizComponent>,
    /// Output channels in order: cmap order when the header carries a cmap
    /// box, component order otherwise.
    channels: Vec<Channel>,
    /// The validated palette, when a usable pclr + cmap pair exists.
    palette: Option<Palette>,
    /// colr semantics, resolved in `finish` (None for raw codestreams).
    color: Option<ColorSpec>,
    /// cdef entries, resolved to `alpha_index` in `finish`.
    channel_definitions: Vec<ChannelDefinition>,
    /// Interleaved 8-bit output, `width * height * channels` bytes;
    /// never-pushed tiles simply stay zero (fail-soft doctrine).
    buffer: Vec<u8>,
    /// Soft findings from `new`/`push_tile`/`finish`, appended after the
    /// decode-wide warnings handed to `finish`.
    warnings: Vec<JpxWarning>,
}

impl ImageAssembler {
    /// Validates the SIZ/JP2-header combination against `limits`
    /// (`max_decoded_bytes` is checked here, BEFORE the output allocation)
    /// and sets up the output canvas. `header` is `None` for raw
    /// codestreams: colour is then guessed from the component count.
    pub(crate) fn new(
        siz: &Siz,
        header: Option<&Jp2Header>,
        limits: &DecodeLimits,
    ) -> Result<ImageAssembler> {
        let region = Rect {
            x0: siz.xosiz,
            y0: siz.yosiz,
            x1: siz.xsiz,
            y1: siz.ysiz,
        };
        let mut warnings = Vec::new();
        // The codestream is the truth: header mismatches warn, SIZ wins.
        // Palette, cdef and colr have no codestream counterpart, so those
        // are taken from the header as-is.
        if let Some(header) = header {
            if (header.width, header.height) != (region.width(), region.height()) {
                warnings.push(JpxWarning::note(format!(
                    "ihdr claims {}x{} but the SIZ image region is {}x{}; SIZ wins",
                    header.width,
                    header.height,
                    region.width(),
                    region.height()
                )));
            }
            if usize::from(header.num_components) != siz.components.len() {
                warnings.push(JpxWarning::note(format!(
                    "ihdr NC = {} but SIZ Csiz = {}; SIZ wins",
                    header.num_components,
                    siz.components.len()
                )));
            }
            // ihdr BPC / bpcc store depth - 1 in the low 7 bits plus a sign
            // MSB (I.5.3.1/I.5.3.2) — the same encoding as Ssiz.
            let siz_raw = |spec: &SizComponent| {
                (spec.depth.saturating_sub(1) & 127) | if spec.signed { 128 } else { 0 }
            };
            let mismatch = if header.bit_depth == 255 {
                header.component_depths.len() == siz.components.len()
                    && header
                        .component_depths
                        .iter()
                        .zip(&siz.components)
                        .any(|(&raw, spec)| raw != siz_raw(spec))
            } else {
                siz.components
                    .iter()
                    .any(|spec| siz_raw(spec) != header.bit_depth)
            };
            if mismatch {
                warnings.push(JpxWarning::note(
                    "ihdr/bpcc bit depth disagrees with SIZ Ssiz; SIZ wins",
                ));
            }
        }
        let (channels, palette) = build_channels(siz, header, &mut warnings);
        if channels.is_empty() {
            return Err(JpxError::Malformed(
                "no output channels (empty Csiz and cmap)".into(),
            ));
        }
        // DecodedImage::components is a u8; more channels than that cannot
        // be represented and header-level problems are hard errors.
        if channels.len() > usize::from(u8::MAX) {
            return Err(JpxError::Malformed(format!(
                "{} output channels exceed the representable 255",
                channels.len()
            )));
        }
        let bytes = u64::from(region.width()) * u64::from(region.height()) * channels.len() as u64;
        if bytes > limits.max_decoded_bytes {
            return Err(JpxError::LimitExceeded {
                what: "max_decoded_bytes",
                actual: bytes,
                limit: limits.max_decoded_bytes,
            });
        }
        let len = usize::try_from(bytes).map_err(|convert_error| {
            JpxError::Malformed(format!(
                "output buffer exceeds the address space: {convert_error}"
            ))
        })?;
        Ok(ImageAssembler {
            region,
            components: siz.components.clone(),
            channels,
            palette,
            color: header.map(|header| header.color.clone()),
            channel_definitions: header
                .map(|header| header.channel_definitions.clone())
                .unwrap_or_default(),
            buffer: vec![0; len],
            warnings,
        })
    }

    /// Composites one decoded tile. `tile` is the reference-grid tile rect
    /// (B-7..B-10); `mct` is the tile's Table A.17 flag; `mct_wavelet` is
    /// the common wavelet filter of components 0..3 (`None` when they
    /// disagree or fewer than three exist), which selects the transform
    /// Table A.17 pairs with the flag; `canvases` arrive in codestream
    /// component order, each at its absolute tile-component rect (B-12)
    /// on its own component grid.
    pub(crate) fn push_tile(
        &mut self,
        tile: Rect,
        mct: u8,
        mct_wavelet: Option<WaveletKind>,
        canvases: Vec<TileComponentCanvas>,
    ) -> Result<()> {
        // Clip to the image region (B-1); tiles never exceed it when the
        // caller uses (B-7)..(B-10), but the rect is hostile here.
        let clipped = Rect {
            x0: tile.x0.max(self.region.x0),
            y0: tile.y0.max(self.region.y0),
            x1: tile.x1.min(self.region.x1),
            y1: tile.y1.min(self.region.y1),
        };
        if clipped.is_empty() {
            return Ok(());
        }
        if canvases.len() != self.components.len() {
            // A skipped tile is missing pixels.
            self.warnings.push(JpxWarning::loss(format!(
                "tile at ({}, {}): {} component canvases for {} components; tile skipped",
                tile.x0,
                tile.y0,
                canvases.len(),
                self.components.len()
            )));
            return Ok(());
        }
        // Widen every canvas for transform arithmetic; a canvas whose
        // sample count contradicts its rect degrades to an empty one (its
        // channels keep their zeros).
        let mut work: Vec<(Rect, WorkCanvas)> = Vec::with_capacity(canvases.len());
        for (index, canvas) in canvases.into_iter().enumerate() {
            let expected = u64::from(canvas.rect.width()) * u64::from(canvas.rect.height());
            let actual = match &canvas.samples {
                CoefficientCanvas::Reversible(values) => values.len(),
                CoefficientCanvas::Irreversible(values) => values.len(),
            } as u64;
            if actual != expected {
                // A zero-filled component is missing pixels.
                self.warnings.push(JpxWarning::loss(format!(
                    "tile at ({}, {}): component {index} canvas holds {actual} samples \
                     for a {}x{} rect; component zero-filled",
                    tile.x0,
                    tile.y0,
                    canvas.rect.width(),
                    canvas.rect.height()
                )));
                work.push((
                    Rect {
                        x0: 0,
                        y0: 0,
                        x1: 0,
                        y1: 0,
                    },
                    WorkCanvas::Int(Vec::new()),
                ));
                continue;
            }
            let widened = match canvas.samples {
                CoefficientCanvas::Reversible(values) => {
                    WorkCanvas::Int(values.into_iter().map(i64::from).collect())
                }
                CoefficientCanvas::Irreversible(values) => {
                    WorkCanvas::Float(values.into_iter().map(f64::from).collect())
                }
            };
            work.push((canvas.rect, widened));
        }
        // Undo the multiple component transformation (Table A.17).
        match mct {
            0 => {}
            1 => apply_inverse_mct(&mut work, mct_wavelet, &mut self.warnings),
            // Whatever transform the reserved value meant stays applied:
            // the colours cannot be trusted.
            other => self.warnings.push(JpxWarning::loss(format!(
                "reserved SGcod MCT value {other} ignored (Table A.17)"
            ))),
        }
        // Integerize, inverse DC level shift (G-2) and clamp per component.
        let planes: Vec<(Rect, Vec<i64>)> = work
            .into_iter()
            .zip(&self.components)
            .map(|((rect, canvas), spec)| {
                let ints: Vec<i64> = match canvas {
                    WorkCanvas::Int(values) => values,
                    // Round half away from zero (f64::round); the `as`
                    // cast saturates, so hostile non-finite values stay
                    // total (NaN becomes 0).
                    WorkCanvas::Float(values) => {
                        values.into_iter().map(|v| v.round() as i64).collect()
                    }
                };
                let shifted = ints
                    .into_iter()
                    .map(|v| level_shift_and_clamp(v, spec.depth, spec.signed))
                    .collect();
                (rect, shifted)
            })
            .collect();
        // Per output channel: normalize at component resolution, then blit
        // with replication onto the image region.
        for index in 0..self.channels.len() {
            let channel = self.channels[index];
            let (component, bytes) = match channel.source {
                ChannelSource::Zero => continue,
                ChannelSource::Direct { component } => {
                    let (_, plane) = &planes[component];
                    let bytes: Vec<u8> = plane
                        .iter()
                        .map(|&v| normalize_to_u8(v, channel.depth, channel.signed))
                        .collect();
                    (component, bytes)
                }
                ChannelSource::Palette { component, column } => {
                    let Some(palette) = &self.palette else {
                        continue;
                    };
                    let (_, plane) = &planes[component];
                    let bytes: Vec<u8> = plane
                        .iter()
                        .map(|&v| {
                            normalize_to_u8(
                                palette_value(palette, v, column),
                                channel.depth,
                                channel.signed,
                            )
                        })
                        .collect();
                    (component, bytes)
                }
            };
            let src_rect = planes[component].0;
            let spec = self.components[component];
            let separation = (u32::from(spec.xrsiz.max(1)), u32::from(spec.yrsiz.max(1)));
            self.blit_replicated(index, clipped, src_rect, &bytes, separation);
        }
        Ok(())
    }

    /// Writes one normalized channel plane into the interleaved buffer.
    /// Reference-grid point (x, y) takes component sample
    /// (floor(x / XRsiz), floor(y / YRsiz)) clamped into the canvas rect —
    /// component samples sit on every XRsiz-th/YRsiz-th grid point (B.2),
    /// and replication fills the points in between. All accesses are
    /// bounds-checked (hostile rects at worst write nothing).
    fn blit_replicated(
        &mut self,
        channel: usize,
        dst: Rect,
        src_rect: Rect,
        src: &[u8],
        separation: (u32, u32),
    ) {
        if src_rect.is_empty() {
            return;
        }
        let stride = self.channels.len();
        let width = self.region.width() as usize;
        let src_width = src_rect.width() as usize;
        for y in dst.y0..dst.y1 {
            let v = (y / separation.1).clamp(src_rect.y0, src_rect.y1 - 1);
            let src_row = (v - src_rect.y0) as usize * src_width;
            let dst_row = (y - self.region.y0) as usize * width;
            for x in dst.x0..dst.x1 {
                let u = (x / separation.0).clamp(src_rect.x0, src_rect.x1 - 1);
                let sample = src
                    .get(src_row + (u - src_rect.x0) as usize)
                    .copied()
                    .unwrap_or(0);
                let index = (dst_row + (x - self.region.x0) as usize) * stride + channel;
                if let Some(slot) = self.buffer.get_mut(index) {
                    *slot = sample;
                }
            }
        }
    }

    /// Counts the colour channels: every channel not declared an opacity
    /// channel (cdef Typ 1 or 2, I.5.3.6) — with no cdef box, all of them.
    fn colour_channel_count(&self) -> u8 {
        let mut opacity: Vec<u16> = self
            .channel_definitions
            .iter()
            .filter(|def| def.kind == 1 || def.kind == 2)
            .map(|def| def.channel)
            .filter(|&channel| usize::from(channel) < self.channels.len())
            .collect();
        opacity.sort_unstable();
        opacity.dedup();
        // opacity only holds in-range channel indices, so this cannot
        // underflow, and new() capped the channel count at 255.
        (self.channels.len() - opacity.len()) as u8
    }

    /// Resolves the cdef opacity channel (I.5.3.6): the first channel of
    /// type 1 or 2 associated with the whole image (Asoc 0). Premultiplied
    /// opacity (Typ 2) is reported but the samples stay premultiplied —
    /// un-premultiplying is the consumer's decision.
    fn resolve_alpha(&mut self) -> Option<u8> {
        for index in 0..self.channel_definitions.len() {
            let def = self.channel_definitions[index];
            if (def.kind != 1 && def.kind != 2) || def.association != 0 {
                continue;
            }
            if usize::from(def.channel) >= self.channels.len() {
                self.warnings.push(JpxWarning::note(format!(
                    "cdef opacity channel {} does not exist; ignored",
                    def.channel
                )));
                continue;
            }
            if def.kind == 2 {
                self.warnings.push(JpxWarning::note(
                    "cdef declares premultiplied opacity (Typ 2); samples are left premultiplied",
                ));
            }
            return Some(def.channel as u8);
        }
        None
    }

    /// Takes the METH = 2 profile bytes out of the colr declaration, for
    /// export on `DecodedImage::icc_profile`; `None` when the box was not
    /// ICC or the bytes were not carried (the empty-profile sentinel).
    fn take_icc_profile(&mut self) -> Option<Vec<u8>> {
        match &mut self.color {
            Some(ColorSpec::Icc { profile }) if !profile.is_empty() => {
                Some(std::mem::take(profile))
            }
            _ => None,
        }
    }

    /// Resolves the colr box (I.5.3.3) into a [`ColorKind`], converting
    /// sYCC in place. Raw codestreams guess from the component count.
    /// `icc` is the profile [`Self::take_icc_profile`] extracted, consulted
    /// only for the warning wording.
    fn resolve_color(&mut self, icc: Option<&[u8]>) -> ColorKind {
        let colour = self.colour_channel_count();
        match &self.color {
            None => match self.channels.len() {
                1 => ColorKind::Gray,
                3 => ColorKind::Rgb,
                4 => ColorKind::Cmyk,
                count => {
                    self.warnings.push(JpxWarning::note(format!(
                        "raw codestream with {count} components has no colour interpretation"
                    )));
                    // EnumCS 0 is reserved (Table I.10); it stands in for
                    // "unknown" here since no colr box exists to cite.
                    ColorKind::Other {
                        enumeration: 0,
                        components: colour,
                    }
                }
            },
            // Table I.10: 16 = sRGB, 17 = greyscale, 18 = sYCC.
            Some(ColorSpec::Enumerated(16)) => ColorKind::Rgb,
            Some(ColorSpec::Enumerated(17)) => ColorKind::Gray,
            Some(ColorSpec::Enumerated(18)) => {
                if self.channels.len() >= 3 {
                    self.convert_sycc_to_rgb();
                    ColorKind::Rgb
                } else {
                    self.warnings.push(JpxWarning::note(
                        "colr declares sYCC but fewer than three channels exist; left unconverted",
                    ));
                    ColorKind::Other {
                        enumeration: 18,
                        components: colour,
                    }
                }
            }
            Some(ColorSpec::Enumerated(enumeration)) => {
                let enumeration = *enumeration;
                self.warnings.push(JpxWarning::note(format!(
                    "colr enumeration {enumeration} is not converted (Table I.10 defines 16/17/18)"
                )));
                ColorKind::Other {
                    enumeration,
                    components: colour,
                }
            }
            Some(ColorSpec::Icc { .. }) => {
                let message = match icc {
                    Some(profile) => format!(
                        "colr carries a restricted ICC profile ({} bytes), exported \
                         for the consumer to apply; colour reported as a guess from \
                         {colour} colour channels",
                        profile.len()
                    ),
                    None => format!(
                        "colr declares a restricted ICC profile the scan did not \
                         carry; colour guessed from {colour} colour channels"
                    ),
                };
                self.warnings.push(JpxWarning::note(message));
                ColorKind::IccGuess { components: colour }
            }
        }
    }

    /// Converts the first three channels from sYCC to RGB in place. T.800
    /// defines sYCC only by reference (Table I.10, EnumCS 18 -> IEC
    /// 61966-2-1 Amd. 1) and supplies no matrix of its own; the G.3.2
    /// inverse ICT matrix applied to midpoint-centred (-128) 8-bit chroma
    /// is the closest in-spec approximation, and the output is flagged as
    /// approximate.
    fn convert_sycc_to_rgb(&mut self) {
        self.warnings.push(JpxWarning::note(
            "sYCC converted to RGB with the G.3.2 matrix (approximate)",
        ));
        let stride = self.channels.len();
        for pixel in self.buffer.chunks_mut(stride) {
            let y = f64::from(pixel[0]);
            let cb = f64::from(pixel[1]) - 128.0;
            let cr = f64::from(pixel[2]) - 128.0;
            // (G-12)..(G-14) with (Y0, Y1, Y2) = (Y, Cb, Cr).
            let (r, g, b) = inverse_ict(y, cb, cr);
            pixel[0] = clamp_round_u8(r);
            pixel[1] = clamp_round_u8(g);
            pixel[2] = clamp_round_u8(b);
        }
    }

    /// Finalizes the image, attaching the accumulated `warnings`.
    pub(crate) fn finish(mut self, warnings: Vec<JpxWarning>) -> Result<DecodedImage> {
        let mut all = warnings;
        let icc_profile = self.take_icc_profile();
        let color = self.resolve_color(icc_profile.as_deref());
        let alpha_index = self.resolve_alpha();
        all.append(&mut self.warnings);
        Ok(DecodedImage {
            width: self.region.width(),
            height: self.region.height(),
            // new() rejected channel counts above 255.
            components: self.channels.len() as u8,
            samples: self.buffer,
            // The pre-normalization source depth per output channel: the
            // palette column's depth for palette-mapped channels, the
            // component's Ssiz depth otherwise (DecodedImage contract).
            component_depths: self.channels.iter().map(|channel| channel.depth).collect(),
            color,
            icc_profile,
            alpha_index,
            warnings: all,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{ChannelDefinition, ColorSpec, ComponentMapping, Palette};
    use crate::dequant::CoefficientCanvas;
    use crate::markers::SizComponent;
    use crate::ColorKind;

    // ------------------------------------------------------------------
    // Construction helpers
    // ------------------------------------------------------------------

    fn component(depth: u8, signed: bool, xrsiz: u8, yrsiz: u8) -> SizComponent {
        SizComponent {
            depth,
            signed,
            xrsiz,
            yrsiz,
        }
    }

    fn siz_for(
        xsiz: u32,
        ysiz: u32,
        xosiz: u32,
        yosiz: u32,
        xtsiz: u32,
        ytsiz: u32,
        components: Vec<SizComponent>,
    ) -> Siz {
        Siz {
            rsiz: 0,
            xsiz,
            ysiz,
            xosiz,
            yosiz,
            xtsiz,
            ytsiz,
            xtosiz: 0,
            ytosiz: 0,
            components,
        }
    }

    fn rect(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn reversible(rect: Rect, values: &[i32]) -> TileComponentCanvas {
        TileComponentCanvas {
            rect,
            levels: 0,
            samples: CoefficientCanvas::Reversible(values.to_vec()),
        }
    }

    fn irreversible(rect: Rect, values: &[f32]) -> TileComponentCanvas {
        TileComponentCanvas {
            rect,
            levels: 0,
            samples: CoefficientCanvas::Irreversible(values.to_vec()),
        }
    }

    fn plain_header(color: ColorSpec, num_components: u16, width: u32, height: u32) -> Jp2Header {
        Jp2Header {
            height,
            width,
            num_components,
            // Raw B value 7 = 8-bit unsigned (I.5.3.1: depth - 1, sign bit clear).
            bit_depth: 7,
            component_depths: Vec::new(),
            color,
            palette: None,
            component_mapping: Vec::new(),
            channel_definitions: Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // (1) Inverse RCT — G.2.2, Equations (G-6)..(G-8)
    // ------------------------------------------------------------------

    /// Forward RCT per (G-3)..(G-5), test-side only:
    /// Y0 = floor((I0 + 2*I1 + I2)/4), Y1 = I2 - I1, Y2 = I0 - I1.
    fn forward_rct(i0: i64, i1: i64, i2: i64) -> (i64, i64, i64) {
        ((i0 + 2 * i1 + i2).div_euclid(4), i2 - i1, i0 - i1)
    }

    #[test]
    fn inverse_rct_reproduces_the_g6_g8_hand_vectors() {
        // (I0, I1, I2) = (100, 50, 25):
        //   (G-3) Y0 = floor((100 + 2*50 + 25)/4) = floor(225/4) = 56
        //   (G-4) Y1 = 25 - 50 = -25;   (G-5) Y2 = 100 - 50 = 50
        // Inverse: (G-6) I1 = 56 - floor((50 + (-25))/4) = 56 - 6 = 50
        //          (G-7) I0 = 50 + 50 = 100;  (G-8) I2 = -25 + 50 = 25
        assert_eq!(inverse_rct(56, -25, 50), (100, 50, 25));

        // Negative chroma exercising the negative floor division:
        // (I0, I1, I2) = (-100, 30, -50):
        //   Y0 = floor((-100 + 60 - 50)/4) = floor(-90/4) = floor(-22.5) = -23
        //   Y1 = -50 - 30 = -80;  Y2 = -100 - 30 = -130
        // Inverse: I1 = -23 - floor((-130 + -80)/4) = -23 - floor(-52.5)
        //             = -23 - (-53) = 30
        //          I0 = -130 + 30 = -100;  I2 = -80 + 30 = -50
        assert_eq!(inverse_rct(-23, -80, -130), (-100, 30, -50));

        // Small positive numerator: (Y0, Y1, Y2) = (0, 1, 0):
        //   I1 = 0 - floor(1/4) = 0;  I0 = 0 + 0 = 0;  I2 = 1 + 0 = 1
        assert_eq!(inverse_rct(0, 1, 0), (0, 0, 1));
    }

    #[test]
    fn inverse_rct_round_trips_the_forward_transform() {
        // Reversibility (G.2): inverse(forward(x)) == x, bit exact, for
        // signed 8-bit-shifted samples including the extremes.
        for i0 in [-128i64, -77, -1, 0, 1, 89, 127] {
            for i1 in [-128i64, -3, 0, 42, 127] {
                for i2 in [-128i64, -90, 0, 5, 127] {
                    let (y0, y1, y2) = forward_rct(i0, i1, i2);
                    assert_eq!(
                        inverse_rct(y0, y1, y2),
                        (i0, i1, i2),
                        "round trip of ({i0}, {i1}, {i2})"
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // (2) Inverse ICT — G.3.2, Equations (G-12)..(G-14)
    // ------------------------------------------------------------------

    /// Forward ICT per (G-9)..(G-11), test-side only. As printed, (G-9)
    /// reads Y0 = -0.299 I0 - 0.587 I1 + 0.114 I2, which the inverse
    /// (G-12)..(G-14) does NOT invert (substituting (G-12)..(G-14) back
    /// requires Y0 = 0.299 I0 + 0.587 I1 + 0.114 I2); the positive luma
    /// signs are used so the forward/inverse pair is self-consistent.
    fn forward_ict(i0: f64, i1: f64, i2: f64) -> (f64, f64, f64) {
        (
            0.299 * i0 + 0.587 * i1 + 0.114 * i2, // (G-9), signs corrected
            -0.16875 * i0 - 0.331260 * i1 + 0.5 * i2, // (G-10)
            0.5 * i0 - 0.41869 * i1 - 0.08131 * i2, // (G-11)
        )
    }

    #[test]
    fn inverse_ict_reproduces_the_g12_g14_hand_vectors() {
        // (Y0, Y1, Y2) = (100, -20, 30):
        //   (G-12) I0 = 100 + 1.402*30 = 100 + 42.06        = 142.060000
        //   (G-13) I1 = 100 - 0.34413*(-20) - 0.71414*30
        //             = 100 + 6.882600 - 21.424200          =  85.458400
        //   (G-14) I2 = 100 + 1.772*(-20) = 100 - 35.44     =  64.560000
        let (i0, i1, i2) = inverse_ict(100.0, -20.0, 30.0);
        assert!((i0 - 142.060000).abs() < 1e-6, "I0 = {i0}");
        assert!((i1 - 85.458400).abs() < 1e-6, "I1 = {i1}");
        assert!((i2 - 64.560000).abs() < 1e-6, "I2 = {i2}");

        // (Y0, Y1, Y2) = (50.5, 10.25, -5.75):
        //   I0 = 50.5 + 1.402*(-5.75) = 50.5 - 8.0615       = 42.438500
        //   I1 = 50.5 - 0.34413*10.25 - 0.71414*(-5.75)
        //      = 50.5 - 3.5273325 + 4.1063050               = 51.078973
        //   I2 = 50.5 + 1.772*10.25 = 50.5 + 18.163         = 68.663000
        let (i0, i1, i2) = inverse_ict(50.5, 10.25, -5.75);
        assert!((i0 - 42.438500).abs() < 1e-6, "I0 = {i0}");
        assert!((i1 - 51.0789725).abs() < 1e-6, "I1 = {i1}");
        assert!((i2 - 68.663000).abs() < 1e-6, "I2 = {i2}");
    }

    #[test]
    fn inverse_ict_round_trips_within_half_a_code_value() {
        // The published coefficient pairs are rounded to 5-6 decimals, so
        // forward + inverse is not exact — but for 8-bit data the error
        // stays far below 0.51, i.e., rounding recovers every code value.
        for r in [0.0f64, 1.0, 63.0, 127.0, 128.0, 254.0, 255.0] {
            for g in [0.0f64, 50.0, 128.0, 255.0] {
                for b in [0.0f64, 99.0, 200.0, 255.0] {
                    let (y0, y1, y2) = forward_ict(r, g, b);
                    let (i0, i1, i2) = inverse_ict(y0, y1, y2);
                    assert!((i0 - r).abs() <= 0.51, "R {r} came back as {i0}");
                    assert!((i1 - g).abs() <= 0.51, "G {g} came back as {i1}");
                    assert!((i2 - b).abs() <= 0.51, "B {b} came back as {i2}");
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // (3) DC level shift (G-2) + depth normalization hand-cases
    // ------------------------------------------------------------------

    #[test]
    fn dc_level_shift_recentres_unsigned_components() {
        // (G-2): unsigned samples gain 2^(Ssiz) with Ssiz = depth - 1.
        // depth 8: +2^7 = +128.
        assert_eq!(level_shift_and_clamp(-128, 8, false), 0);
        assert_eq!(level_shift_and_clamp(127, 8, false), 255);
        // Out-of-range reconstructions clip to the component range, the
        // G.1.2 NOTE's "typical solution": 200 + 128 = 328 -> 255.
        assert_eq!(level_shift_and_clamp(200, 8, false), 255);
        assert_eq!(level_shift_and_clamp(-300, 8, false), 0);
        // Signed components are NOT shifted (G.1.2 shifts unsigned only),
        // just clamped to [-2^(d-1), 2^(d-1) - 1] = [-128, 127] for d = 8.
        assert_eq!(level_shift_and_clamp(127, 8, true), 127);
        assert_eq!(level_shift_and_clamp(-200, 8, true), -128);
        // depth 1: +2^0 = +1; depth 12: +2^11 = +2048; depth 16: +2^15.
        assert_eq!(level_shift_and_clamp(-1, 1, false), 0);
        assert_eq!(level_shift_and_clamp(0, 1, false), 1);
        assert_eq!(level_shift_and_clamp(-2048, 12, false), 0);
        assert_eq!(level_shift_and_clamp(2047, 12, false), 4095);
        // -6716 + 32768 = 26052 (the 16-bit fixture's last sample).
        assert_eq!(level_shift_and_clamp(-6716, 16, false), 26052);
    }

    #[test]
    fn normalization_hand_cases_for_depths_1_4_8_12_16() {
        // depth 8: pass-through of the already-clamped 0..255 range.
        assert_eq!(normalize_to_u8(0, 8, false), 0);
        assert_eq!(normalize_to_u8(255, 8, false), 255);
        // depth 8 signed: +2^7 recentre. -128 -> 0; 127 -> 255.
        assert_eq!(normalize_to_u8(-128, 8, true), 0);
        assert_eq!(normalize_to_u8(127, 8, true), 255);
        // depth 12 (> 8): rounded right shift by 12 - 8 = 4, i.e.
        // (v + 8) >> 4 capped at 255.
        // 4095 -> 4103 >> 4 = 256, saturated 255; 2048 -> 2056 >> 4 = 128
        // (128,5 floors to 128); 100 -> 108 >> 4 = 6 (6,75 floors to 6).
        assert_eq!(normalize_to_u8(4095, 12, false), 255);
        assert_eq!(normalize_to_u8(2048, 12, false), 128);
        assert_eq!(normalize_to_u8(100, 12, false), 6);
        // depth 12 signed: -2048 + 2048 = 0 -> 0;
        // 100 + 2048 = 2148 -> 2156 >> 4 = 134 (134,75 floors to 134).
        assert_eq!(normalize_to_u8(-2048, 12, true), 0);
        assert_eq!(normalize_to_u8(100, 12, true), 134);
        // depth 16: rounded shift by 8, (v + 128) >> 8 capped at 255.
        // 65535 -> 65663 >> 8 = 256, saturated 255;
        // 26052 -> 26180 >> 8 = 102 (26052 / 256 = 101,77 rounds UP —
        // truncation gave 101); 258 -> 386 >> 8 = 1 (1,5 floors to 1).
        assert_eq!(normalize_to_u8(65535, 16, false), 255);
        assert_eq!(normalize_to_u8(26052, 16, false), 102);
        assert_eq!(normalize_to_u8(258, 16, false), 1);
        // A value where rounding and truncation disagree, pinned:
        // 25800 / 256 = 100,78 -> 101; a plain 25800 >> 8 gave 100.
        assert_eq!(normalize_to_u8(25800, 16, false), 101);
        // The midpoint invariant the sYCC chroma centring relies on:
        // 2^(d-1) -> exactly 128 at every deep depth.
        for depth in 9..=32u8 {
            let mid = 1i64 << (depth - 1);
            assert_eq!(normalize_to_u8(mid, depth, false), 128, "depth {depth}");
        }
        // depth 16 signed: 0 + 32768 = 32768 -> 32896 >> 8 = 128 (the
        // recentred midpoint, 128,5 flooring to 128).
        assert_eq!(normalize_to_u8(0, 16, true), 128);
        assert_eq!(normalize_to_u8(-32768, 16, true), 0);
        // depth 1 (< 8): scale by round(v*255/(2^1 - 1)) = v*255.
        assert_eq!(normalize_to_u8(0, 1, false), 0);
        assert_eq!(normalize_to_u8(1, 1, false), 255);
        // depth 1 signed: -1 + 1 = 0 -> 0; 0 + 1 = 1 -> 255.
        assert_eq!(normalize_to_u8(-1, 1, true), 0);
        assert_eq!(normalize_to_u8(0, 1, true), 255);
        // depth 4: round(v*255/15) = v*17 exactly.
        // 15 -> 255; 7 -> floor((7*255 + 7)/15) = floor(1792/15) = 119; 1 -> 17.
        assert_eq!(normalize_to_u8(15, 4, false), 255);
        assert_eq!(normalize_to_u8(7, 4, false), 119);
        assert_eq!(normalize_to_u8(1, 4, false), 17);
        // depth 4 signed: 3 + 8 = 11 -> floor((11*255 + 7)/15) = 187.
        assert_eq!(normalize_to_u8(3, 4, true), 187);
    }

    // ------------------------------------------------------------------
    // (4) Palette expansion — I.5.3.4 / I.5.3.5
    // ------------------------------------------------------------------

    #[test]
    fn palette_expansion_maps_indices_through_each_cmap_entry() {
        let siz = siz_for(5, 1, 0, 0, 5, 1, vec![component(8, false, 1, 1)]);
        let mut header = plain_header(ColorSpec::Enumerated(16), 1, 5, 1);
        header.palette = Some(Palette {
            entries: 4,
            created_channels: 2,
            // B_0 raw 7 = 8-bit unsigned; B_1 raw 3 = 4-bit unsigned
            // (Table I.13: depth = low-7-bits + 1, MSB = sign).
            channel_depths: vec![7, 3],
            // Entry-major layout (I.5.3.4): entry j holds (col 0, col 1).
            values: vec![10, 1, 20, 5, 30, 15, 255, 0],
        });
        header.component_mapping = vec![
            // Channel 0 <- palette column 0 of component 0 (MTYP 1, I.5.3.5).
            ComponentMapping {
                component: 0,
                mapping_type: 1,
                palette_column: 0,
            },
            // Channel 1 <- palette column 1 of component 0.
            ComponentMapping {
                component: 0,
                mapping_type: 1,
                palette_column: 1,
            },
        ];
        let mut assembler =
            ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        // Decoded (pre-DC-shift) values -128..-124 and -119 gain +128 (G-2)
        // and become palette indices 0, 1, 2, 3, 9; index 9 >= NE = 4 is
        // hostile and clamps to the last entry (3).
        assembler
            .push_tile(
                rect(0, 0, 5, 1),
                0,
                None,
                vec![reversible(
                    rect(0, 0, 5, 1),
                    &[-128, -127, -126, -125, -119],
                )],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.components, 2);
        // component_depths reports the PALETTE COLUMN depths (Table I.13),
        // not the 8-bit index component's — the contract that lets a PDF
        // renderer reverse the normalization (ISO 32000 7.4.9).
        assert_eq!(image.component_depths, vec![8, 4]);
        // Column 0 (8-bit) passes through; column 1 (4-bit) scales by
        // round(v*255/15) = v*17:
        //   idx 0 -> (10, 1*17 = 17);   idx 1 -> (20,  5*17 = 85)
        //   idx 2 -> (30, 15*17 = 255); idx 3 -> (255, 0*17 = 0)
        //   idx 9 -> clamped to 3 -> (255, 0)
        assert_eq!(image.samples, vec![10, 17, 20, 85, 30, 255, 255, 0, 255, 0]);
    }

    // ------------------------------------------------------------------
    // (5) Multi-tile composition with a nonzero image-region offset
    // ------------------------------------------------------------------

    #[test]
    fn tiles_compose_at_their_image_region_offsets() {
        // SIZ: Xsiz=8, Ysiz=5, XOsiz=2, YOsiz=1, XTsiz=4, YTsiz=5.
        // (B-5): numXtiles = ceil(8/4) = 2, numYtiles = ceil(5/5) = 1.
        // (B-7)..(B-10): tile 0 = [max(0,2),4) x [max(0,1),5) = [2,4)x[1,5);
        //                tile 1 = [4,8)x[1,5).
        // Image region (B-1): [2,8)x[1,5) -> 6 x 4 output samples.
        let siz = siz_for(8, 5, 2, 1, 4, 5, vec![component(8, false, 1, 1)]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        // Tile 1 first: raster sample k decodes as k - 28, +128 = 100 + k.
        let values: Vec<i32> = (0..16).map(|k| k - 28).collect();
        assembler
            .push_tile(
                rect(4, 1, 8, 5),
                0,
                None,
                vec![reversible(rect(4, 1, 8, 5), &values)],
            )
            .unwrap();
        // Tile 0: constant decode -121, +128 = 7.
        assembler
            .push_tile(
                rect(2, 1, 4, 5),
                0,
                None,
                vec![reversible(rect(2, 1, 4, 5), &[-121; 8])],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!((image.width, image.height), (6, 4));
        // Absolute (x, y) lands at buffer[(y - YOsiz)*6 + (x - XOsiz)]:
        // tile-1 pixel (4, 1) -> buffer[0*6 + 2] = 2; its second row starts
        // at (4, 2) -> buffer[1*6 + 2] = 8.
        assert_eq!(image.samples[2], 100);
        assert_eq!(image.samples[3], 101);
        assert_eq!(image.samples[4], 102);
        assert_eq!(image.samples[5], 103);
        assert_eq!(image.samples[8], 104);
        // Tile 0 fills columns 0..2 of every row.
        assert_eq!(image.samples[0], 7);
        assert_eq!(image.samples[1], 7);
        assert_eq!(image.samples[6], 7);
        assert_eq!(image.samples[7], 7);
        // Full raster: two tile-0 columns then four tile-1 columns per row.
        let expected: Vec<u8> = (0..4u8)
            .flat_map(|row| {
                let mut r = vec![7u8, 7];
                r.extend((0..4u8).map(|col| 100 + 4 * row + col));
                r
            })
            .collect();
        assert_eq!(image.samples, expected);
    }

    #[test]
    fn missing_tiles_leave_zeros() {
        let siz = siz_for(8, 5, 2, 1, 4, 5, vec![component(8, false, 1, 1)]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        assembler
            .push_tile(
                rect(2, 1, 4, 5),
                0,
                None,
                vec![reversible(rect(2, 1, 4, 5), &[-121; 8])],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        // Tile 1 ([4,8)x[1,5)) was never pushed: its samples stay zero.
        assert_eq!(image.samples[2], 0);
        assert_eq!(image.samples[5], 0);
        assert_eq!(image.samples[0], 7);
    }

    // ------------------------------------------------------------------
    // (6) Replication upsampling of subsampled components (B.2/G.4)
    // ------------------------------------------------------------------

    #[test]
    fn subsampled_components_replicate_onto_the_reference_grid() {
        // XRsiz = 2: the component grid is the reference grid ceil-divided
        // by 2 (B-12), so the 4 x 2 reference tile holds a 2 x 2 component
        // canvas; component column u covers reference columns 2u and 2u+1
        // (B.2 places component samples every XRsiz-th reference column).
        let siz = siz_for(4, 2, 0, 0, 4, 2, vec![component(8, false, 2, 1)]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        // Decoded -118, -108, -98, -88, +128 (G-2) = 10, 20, 30, 40.
        assembler
            .push_tile(
                rect(0, 0, 4, 2),
                0,
                None,
                vec![reversible(rect(0, 0, 2, 2), &[-118, -108, -98, -88])],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!((image.width, image.height), (4, 2));
        assert_eq!(image.samples, vec![10, 10, 20, 20, 30, 30, 40, 40]);
    }

    // ------------------------------------------------------------------
    // End-to-end MCT paths through push_tile
    // ------------------------------------------------------------------

    #[test]
    fn mct_1_with_reversible_canvases_undoes_the_rct() {
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 3]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        // Original RGB (100, 50, 25); forward DC shift (G-1) subtracts 128:
        // (-28, -78, -103). Forward RCT (G-3)..(G-5):
        //   Y0 = floor((-28 + 2*(-78) + (-103))/4) = floor(-287/4) = -72
        //   Y1 = -103 - (-78) = -25;  Y2 = -28 - (-78) = 50
        // Inverse (G-6)..(G-8): I1 = -72 - floor(25/4) = -78; I0 = -28;
        // I2 = -103; +128 each = (100, 50, 25).
        assembler
            .push_tile(
                rect(0, 0, 1, 1),
                1,
                Some(WaveletKind::Reversible53),
                vec![
                    reversible(rect(0, 0, 1, 1), &[-72]),
                    reversible(rect(0, 0, 1, 1), &[-25]),
                    reversible(rect(0, 0, 1, 1), &[50]),
                ],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.samples, vec![100, 50, 25]);
    }

    #[test]
    fn mct_1_with_irreversible_canvases_undoes_the_ict() {
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 3]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        // Original RGB (200, 100, 50), DC-shifted by -128: (72, -28, -78).
        // Forward ICT (sign-corrected (G-9), (G-10), (G-11)) by hand:
        //   Y0 = 0.299*72 + 0.587*(-28) + 0.114*(-78)
        //      = 21.528 - 16.436 - 8.892                     = -3.8
        //   Y1 = -0.16875*72 - 0.331260*(-28) + 0.5*(-78)
        //      = -12.15 + 9.27528 - 39.0                     = -41.87472
        //   Y2 = 0.5*72 - 0.41869*(-28) - 0.08131*(-78)
        //      = 36 + 11.72332 + 6.34218                     = 54.0655
        // Inverse (G-12)..(G-14): I0 = 71.99983 -> round 72 -> 200;
        // I1 = -27.99999 -> -28 -> 100; I2 = -78.00200 -> -78 -> 50.
        assembler
            .push_tile(
                rect(0, 0, 1, 1),
                1,
                Some(WaveletKind::Irreversible97),
                vec![
                    irreversible(rect(0, 0, 1, 1), &[-3.8]),
                    irreversible(rect(0, 0, 1, 1), &[-41.87472]),
                    irreversible(rect(0, 0, 1, 1), &[54.0655]),
                ],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.samples, vec![200, 100, 50]);
    }

    #[test]
    fn mct_with_mixed_canvas_kinds_is_skipped_with_a_warning() {
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 3]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        // Table A.17 pairs RCT with 5-3 and ICT with 9-7; a mixed set is
        // malformed, so the transform is skipped fail-soft:
        // -28 + 128 = 100; round(0.4) = 0 + 128 = 128; -103 + 128 = 25.
        assembler
            .push_tile(
                rect(0, 0, 1, 1),
                1,
                Some(WaveletKind::Reversible53),
                vec![
                    reversible(rect(0, 0, 1, 1), &[-28]),
                    irreversible(rect(0, 0, 1, 1), &[0.4]),
                    reversible(rect(0, 0, 1, 1), &[-103]),
                ],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.samples, vec![100, 128, 25]);
        assert!(
            image.warnings.iter().any(|w| w.message.contains("mix")),
            "warnings: {:?}",
            image.warnings
        );
    }

    // ------------------------------------------------------------------
    // finish(): colour resolution, sYCC, cdef alpha
    // ------------------------------------------------------------------

    #[test]
    fn sycc_converts_to_rgb_with_an_approximation_warning() {
        let siz = siz_for(2, 1, 0, 0, 2, 1, vec![component(8, false, 1, 1); 3]);
        let header = plain_header(ColorSpec::Enumerated(18), 3, 2, 1);
        let mut assembler =
            ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        // Two pixels, stored (Y, Cb, Cr) after the +128 DC shift:
        //   pixel 0: (100, 128, 128) — neutral chroma;
        //   pixel 1: ( 50, 228,  28).
        assembler
            .push_tile(
                rect(0, 0, 2, 1),
                0,
                None,
                vec![
                    reversible(rect(0, 0, 2, 1), &[-28, -78]),
                    reversible(rect(0, 0, 2, 1), &[0, 100]),
                    reversible(rect(0, 0, 2, 1), &[0, -100]),
                ],
            )
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.color, ColorKind::Rgb);
        assert!(
            image
                .warnings
                .iter()
                .any(|w| w.message.contains("approximate")),
            "warnings: {:?}",
            image.warnings
        );
        // Pixel 0: Cb-128 = Cr-128 = 0 -> R = G = B = Y = 100.
        // Pixel 1: Cb' = 100, Cr' = -100 through the G.3.2 matrix:
        //   R = 50 + 1.402*(-100)                 = -90.2  -> clamp 0
        //   G = 50 - 0.34413*100 - 0.71414*(-100) = 87.001 -> 87
        //   B = 50 + 1.772*100                    = 227.2  -> 227
        assert_eq!(image.samples, vec![100, 100, 100, 0, 87, 227]);
    }

    #[test]
    fn color_kind_defaults_follow_the_component_count_without_a_header() {
        for (count, expected) in [
            (1usize, ColorKind::Gray),
            (3, ColorKind::Rgb),
            (4, ColorKind::Cmyk),
        ] {
            let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); count]);
            let assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
            let image = assembler.finish(Vec::new()).unwrap();
            assert_eq!(image.color, expected, "{count} components");
            assert_eq!(image.alpha_index, None);
        }
        // Two components have no design-doc guess: Other + a warning.
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 2]);
        let assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(
            image.color,
            ColorKind::Other {
                enumeration: 0,
                components: 2
            }
        );
        assert!(!image.warnings.is_empty());
    }

    #[test]
    fn color_kind_follows_the_colr_box() {
        let cases: Vec<(ColorSpec, usize, ColorKind)> = vec![
            (ColorSpec::Enumerated(16), 3, ColorKind::Rgb),
            (ColorSpec::Enumerated(17), 1, ColorKind::Gray),
            (
                ColorSpec::Enumerated(12),
                4,
                ColorKind::Other {
                    enumeration: 12,
                    components: 4,
                },
            ),
            (
                ColorSpec::Icc {
                    profile: vec![9; 20],
                },
                4,
                ColorKind::IccGuess { components: 4 },
            ),
        ];
        for (spec, count, expected) in cases {
            let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); count]);
            let header = plain_header(spec.clone(), count as u16, 1, 1);
            let assembler =
                ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
            let image = assembler.finish(Vec::new()).unwrap();
            assert_eq!(image.color, expected, "{spec:?}");
        }
        // The ICC guess is recorded as a warning, and the profile bytes
        // ride out on the image for the consumer to interpret.
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 4]);
        let header = plain_header(
            ColorSpec::Icc {
                profile: vec![9; 20],
            },
            4,
            1,
            1,
        );
        let assembler = ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.icc_profile, Some(vec![9; 20]));
        assert!(
            image
                .warnings
                .iter()
                .any(|w| !w.data_loss && w.message.contains("ICC profile (20 bytes), exported")),
            "warnings: {:?}",
            image.warnings
        );
        // The empty-profile sentinel (declared but not carried) exports
        // nothing and words the guess accordingly.
        let header = plain_header(
            ColorSpec::Icc {
                profile: Vec::new(),
            },
            4,
            1,
            1,
        );
        let assembler = ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.color, ColorKind::IccGuess { components: 4 });
        assert_eq!(image.icc_profile, None);
        assert!(
            image
                .warnings
                .iter()
                .any(|w| w.message.contains("did not carry")),
            "warnings: {:?}",
            image.warnings
        );
    }

    #[test]
    fn cdef_reports_the_whole_image_opacity_channel() {
        // RGBA with cdef: colours 0..2 (Typ 0, Asoc 1..3 per Table I.18)
        // and channel 3 an opacity channel for the whole image (Typ 1,
        // Asoc 0 per Tables I.16/I.17).
        let siz = siz_for(1, 1, 0, 0, 1, 1, vec![component(8, false, 1, 1); 4]);
        let mut header = plain_header(ColorSpec::Enumerated(16), 4, 1, 1);
        header.channel_definitions = vec![
            ChannelDefinition {
                channel: 0,
                kind: 0,
                association: 1,
            },
            ChannelDefinition {
                channel: 1,
                kind: 0,
                association: 2,
            },
            ChannelDefinition {
                channel: 2,
                kind: 0,
                association: 3,
            },
            ChannelDefinition {
                channel: 3,
                kind: 1,
                association: 0,
            },
        ];
        let assembler = ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.alpha_index, Some(3));
        assert_eq!(image.color, ColorKind::Rgb);

        // Premultiplied opacity (Typ 2): reported, and flagged in a warning
        // — the samples themselves are left premultiplied.
        let mut header = plain_header(ColorSpec::Enumerated(16), 4, 1, 1);
        header.channel_definitions = vec![ChannelDefinition {
            channel: 3,
            kind: 2,
            association: 0,
        }];
        let assembler = ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.alpha_index, Some(3));
        assert!(
            image
                .warnings
                .iter()
                .any(|w| w.message.contains("premultiplied")),
            "warnings: {:?}",
            image.warnings
        );
    }

    // ------------------------------------------------------------------
    // new(): limits and header cross-checks
    // ------------------------------------------------------------------

    #[test]
    fn new_enforces_max_decoded_bytes_before_allocating() {
        // Image region 6 x 4 x 1 component = 24 output bytes.
        let siz = siz_for(8, 5, 2, 1, 4, 5, vec![component(8, false, 1, 1)]);
        let limits = DecodeLimits {
            max_decoded_bytes: 23,
            ..DecodeLimits::default()
        };
        match ImageAssembler::new(&siz, None, &limits) {
            Err(JpxError::LimitExceeded {
                what,
                actual,
                limit,
            }) => {
                assert_eq!(what, "max_decoded_bytes");
                assert_eq!(actual, 24);
                assert_eq!(limit, 23);
            }
            other => panic!("expected max_decoded_bytes breach, got {:?}", other.err()),
        }
        // 24 bytes exactly is fine.
        let limits = DecodeLimits {
            max_decoded_bytes: 24,
            ..DecodeLimits::default()
        };
        assert!(ImageAssembler::new(&siz, None, &limits).is_ok());
    }

    #[test]
    fn siz_wins_over_a_mismatched_ihdr() {
        // SIZ image region is 6 x 4 with one component; the header claims
        // 9 x 9 with two: warnings, and the SIZ numbers win.
        let siz = siz_for(8, 5, 2, 1, 4, 5, vec![component(8, false, 1, 1)]);
        let header = plain_header(ColorSpec::Enumerated(17), 2, 9, 9);
        let assembler = ImageAssembler::new(&siz, Some(&header), &DecodeLimits::default()).unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!((image.width, image.height), (6, 4));
        assert_eq!(image.components, 1);
        assert!(
            image
                .warnings
                .iter()
                .filter(|w| w.message.contains("SIZ"))
                .count()
                >= 2,
            "warnings: {:?}",
            image.warnings
        );
    }

    #[test]
    fn a_tile_with_the_wrong_canvas_count_warns_and_leaves_zeros() {
        let siz = siz_for(2, 1, 0, 0, 2, 1, vec![component(8, false, 1, 1)]);
        let mut assembler = ImageAssembler::new(&siz, None, &DecodeLimits::default()).unwrap();
        assembler
            .push_tile(rect(0, 0, 2, 1), 0, None, Vec::new())
            .unwrap();
        let image = assembler.finish(Vec::new()).unwrap();
        assert_eq!(image.samples, vec![0, 0]);
        assert!(
            image
                .warnings
                .iter()
                .any(|w| w.message.contains("tile skipped")),
            "warnings: {:?}",
            image.warnings
        );
    }
}
