//! Codestream marker segments (ITU-T T.800 Annex A): a cursor over the main
//! and tile-part headers producing typed SIZ/COD/COC/QCD/QCC/RGN/POC/SOT/
//! PPM/PPT values, plus the per-tile / per-component parameter resolution
//! rules of A.6.
//!
//! Parse-and-skip (no typed value, at most a warning): TLM (A.7.1), PLM
//! (A.7.2), PLT (A.7.3), CRG (A.9.1), COM (A.9.2). In-bit-stream markers
//! SOP (A.8.1) / EPH (A.8.2) belong to the packet stage.

use crate::error::{JpxError, Result};
use crate::DecodeLimits;
use crate::JpxWarning;

// Marker codes (Table A.2), in decimal: every marker is 0xFF00 (65280) plus
// the low byte given in the table.
/// Start of codestream, 0xFF4F (Table A.4).
const SOC: u16 = 65359;
/// Image and tile size, 0xFF51 (Table A.9).
const SIZ: u16 = 65361;
/// Coding style default, 0xFF52 (Table A.12).
const COD: u16 = 65362;
/// Coding style component, 0xFF53 (Table A.22).
const COC: u16 = 65363;
/// Tile-part lengths, 0xFF55 (Table A.33).
const TLM: u16 = 65365;
/// Packet length, main header, 0xFF57 (Table A.35).
const PLM: u16 = 65367;
/// Packet length, tile-part header, 0xFF58 (Table A.37).
const PLT: u16 = 65368;
/// Quantization default, 0xFF5C (Table A.27).
const QCD: u16 = 65372;
/// Quantization component, 0xFF5D (Table A.31).
const QCC: u16 = 65373;
/// Region of interest, 0xFF5E (Table A.24).
const RGN: u16 = 65374;
/// Progression order change, 0xFF5F (Table A.32).
const POC: u16 = 65375;
/// Packed packet headers, main header, 0xFF60 (Table A.38).
const PPM: u16 = 65376;
/// Packed packet headers, tile-part header, 0xFF61 (Table A.39).
const PPT: u16 = 65377;
/// Component registration, 0xFF63 (Table A.42).
const CRG: u16 = 65379;
/// Comment, 0xFF64 (Table A.43).
const COM: u16 = 65380;
/// Start of tile-part, 0xFF90 (Table A.5).
const SOT: u16 = 65424;
/// Start of packet, 0xFF91 (Table A.40); in-bit-stream, illegal in headers.
const SOP: u16 = 65425;
/// End of packet header, 0xFF92 (Table A.41); no marker segment parameters.
const EPH: u16 = 65426;
/// Start of data, 0xFF93 (Table A.7).
const SOD: u16 = 65427;
/// End of codestream, 0xFFD9 (Table A.8).
const EOC: u16 = 65497;
/// 0xFF30..=0xFF3F: reserved markers with no marker segment parameters; a
/// decoder shall skip them (A.1.3).
const RESERVED_NO_SEGMENT_FIRST: u16 = 65328;
/// Upper bound of the A.1.3 no-parameter reserved marker range, 0xFF3F.
const RESERVED_NO_SEGMENT_LAST: u16 = 65343;

/// Image and tile size, SIZ (A.5.1, Table A.9). One per codestream,
/// immediately after SOC.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Siz {
    /// Rsiz capability flags (Table A.10). Parsed, kept for warnings only.
    pub rsiz: u16,
    /// Reference grid width Xsiz.
    pub xsiz: u32,
    /// Reference grid height Ysiz.
    pub ysiz: u32,
    /// Image area horizontal offset XOsiz.
    pub xosiz: u32,
    /// Image area vertical offset YOsiz.
    pub yosiz: u32,
    /// Reference tile width XTsiz.
    pub xtsiz: u32,
    /// Reference tile height YTsiz.
    pub ytsiz: u32,
    /// Tile grid horizontal offset XTOsiz.
    pub xtosiz: u32,
    /// Tile grid vertical offset YTOsiz.
    pub ytosiz: u32,
    /// One entry per component, index order (Csiz entries).
    pub components: Vec<SizComponent>,
}

/// Per-component SIZ fields (A.5.1, Tables A.9/A.11).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizComponent {
    /// Sample precision in bits INCLUDING any sign bit: Table A.11 stores
    /// `depth - 1` in Ssiz bits 0-6; the parser applies the `+ 1`.
    pub depth: u8,
    /// Ssiz bit 7: samples are signed two's complement.
    pub signed: bool,
    /// Horizontal separation XRsiz on the reference grid (1..=255).
    pub xrsiz: u8,
    /// Vertical separation YRsiz on the reference grid (1..=255).
    pub yrsiz: u8,
}

/// Progression order (Table A.16), as used by SGcod and Ppoc.
// Constructed by the markers stage; variants mirror Table A.16 verbatim.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ProgressionOrder {
    /// 0: layer-resolution level-component-position.
    Lrcp,
    /// 1: resolution level-layer-component-position.
    Rlcp,
    /// 2: resolution level-position-component-layer.
    Rpcl,
    /// 3: position-component-resolution level-layer.
    Pcrl,
    /// 4: component-position-resolution level-layer.
    Cprl,
}

/// Wavelet filter selector (Table A.20).
// Constructed by the markers stage; variants mirror Table A.20 verbatim.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WaveletKind {
    /// 0: 9-7 irreversible filter (Annex F.3.8.2 lifting, f32 path).
    Irreversible97,
    /// 1: 5-3 reversible filter (Annex F.3.8.1 lifting, i32 path).
    Reversible53,
}

/// Precinct size exponents for one resolution level (Table A.21): precincts
/// span `2^ppx x 2^ppy` on the RESOLUTION grid, anchored at (0, 0).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PrecinctExponents {
    /// PPx (low 4 bits of the Table A.21 byte). Zero only legal at r = 0.
    pub ppx: u8,
    /// PPy (high 4 bits of the Table A.21 byte). Zero only legal at r = 0.
    pub ppy: u8,
}

/// SPcod/SPcoc coding-style parameters (Table A.15).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct CodingStyle {
    /// Number of decomposition levels NL (0..=32; 0 = no transform).
    pub decomposition_levels: u8,
    /// Code-block width exponent xcb (Table A.18 signals `xcb - 2`; the
    /// parser applies the `+ 2`, so this holds the exponent itself).
    pub code_block_width_exp: u8,
    /// Code-block height exponent ycb (same convention as the width).
    /// `xcb + ycb <= 12` per Table A.18.
    pub code_block_height_exp: u8,
    /// Code-block style bit flags, Table A.19: bit 0 selective arithmetic
    /// bypass (D.6), bit 1 context reset, bit 2 termination on each pass
    /// (D.4.1), bit 3 vertically causal contexts (D.7), bit 4 predictable
    /// termination (D.4.2), bit 5 segmentation symbols (D.5).
    pub code_block_style: u8,
    /// Wavelet transformation (Table A.20).
    pub wavelet: WaveletKind,
    /// Per-resolution-level precinct exponents, index == r, length NL + 1,
    /// present when Scod/Scoc bit 0 signals user-defined precincts;
    /// EMPTY means maximal precincts, PPx = PPy = 15 (Table A.13).
    pub precincts: Vec<PrecinctExponents>,
}

/// Coding style default, COD (A.6.1): Scod flags + SGcod + SPcod.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Cod {
    /// Scod bit 1: SOP marker segments may appear before packets (A.8.1).
    pub sop_markers: bool,
    /// Scod bit 2: an EPH marker terminates every packet header (A.8.2).
    pub eph_markers: bool,
    /// SGcod progression order (Table A.16).
    pub progression: ProgressionOrder,
    /// SGcod layer count (1..=65535).
    pub layers: u16,
    /// SGcod multiple component transformation (Table A.17): 0 = none,
    /// 1 = RCT with 5-3 / ICT with 9-7 on components 0, 1, 2.
    pub mct: u8,
    /// SPcod parameters (Table A.15).
    pub style: CodingStyle,
}

/// Coding style component override, COC (A.6.2).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Coc {
    /// Ccoc component index.
    pub component: u16,
    /// SPcoc parameters. COC carries no SGcod: progression/layers/MCT
    /// always come from the governing COD (A.6.2).
    pub style: CodingStyle,
}

/// One quantization step size (Table A.30): 5-bit exponent, 11-bit mantissa.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct QuantStep {
    /// Exponent epsilon_b (Equation (E-3)).
    pub exponent: u8,
    /// Mantissa mu_b (Equation (E-3), denominator 2^11).
    pub mantissa: u16,
}

/// Quantization style (Table A.28, low five bits of Sqcd/Sqcc).
// Constructed by the markers stage; variants mirror Table A.28.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum QuantizationStyle {
    /// "No quantization" (reversible ranging): one exponent per sub-band in
    /// the F.3.1 order (Table A.29; Equation (E-5) derives truncated tails).
    None {
        /// Exponent epsilon_b per sub-band.
        exponents: Vec<u8>,
    },
    /// Scalar derived: a single (exponent, mantissa) pair signalled for the
    /// NL-LL band; all other bands derive via Equation (E-5).
    ScalarDerived {
        /// Exponent epsilon_0 for the NL-LL band.
        exponent: u8,
        /// Mantissa mu_0 for the NL-LL band.
        mantissa: u16,
    },
    /// Scalar expounded: one (exponent, mantissa) pair per sub-band in the
    /// F.3.1 order.
    ScalarExpounded {
        /// Step sizes per sub-band.
        steps: Vec<QuantStep>,
    },
}

/// Parsed QCD/QCC payload (A.6.4/A.6.5, Table A.28).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Quantization {
    /// Guard bit count G (Sqcd bits 5-7), an input to Equation (E-2).
    pub guard_bits: u8,
    /// Style + signalled step sizes.
    pub style: QuantizationStyle,
}

/// One band's resolved quantization parameters: the single source of
/// truth for the Equation (E-5) view of a QCD/QCC segment, shared by the
/// Tier-2 plane budget (Mb, Equation (E-2)) and the dequantization step
/// size (Delta_b, Equation (E-3)) so both stages resolve short lists
/// identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandQuant {
    /// Exponent epsilon_b.
    pub exponent: u32,
    /// Mantissa mu_b (0 in the Table A.28 reversible style, where only
    /// exponents are signalled).
    pub mantissa: u16,
}

impl Quantization {
    /// Resolves (epsilon_b, mu_b) for the band at decomposition level
    /// `level`, flattened F.3.1 index `flat`, of a tile-component with
    /// NL = `levels`. Scalar-derived values always follow Equation (E-5),
    /// `epsilon_b = epsilon_0 - NL + n_b` with `mu_b = mu_0`; the listed
    /// styles take entry `flat` and fall back to the same derivation from
    /// their first entry when the list is short (the derivation clamps at
    /// zero: exponents are unsigned, Table A.30). The parser rejects
    /// empty lists, so a first entry always exists; the `unwrap_or`
    /// defaults are unreachable belt-and-braces.
    pub(crate) fn band_quant(&self, levels: u8, level: u8, flat: usize) -> BandQuant {
        let derive =
            |first: u8| (u32::from(first) + u32::from(level)).saturating_sub(u32::from(levels));
        match &self.style {
            QuantizationStyle::None { exponents } => match exponents.get(flat) {
                Some(&exponent) => BandQuant {
                    exponent: u32::from(exponent),
                    mantissa: 0,
                },
                None => BandQuant {
                    exponent: derive(exponents.first().copied().unwrap_or(0)),
                    mantissa: 0,
                },
            },
            QuantizationStyle::ScalarDerived { exponent, mantissa } => BandQuant {
                exponent: derive(*exponent),
                mantissa: *mantissa,
            },
            QuantizationStyle::ScalarExpounded { steps } => match steps.get(flat) {
                Some(step) => BandQuant {
                    exponent: u32::from(step.exponent),
                    mantissa: step.mantissa,
                },
                None => {
                    let first = steps.first().copied().unwrap_or(QuantStep {
                        exponent: 0,
                        mantissa: 0,
                    });
                    BandQuant {
                        exponent: derive(first.exponent),
                        mantissa: first.mantissa,
                    }
                }
            },
        }
    }

    /// True when a listed style signals fewer entries than the
    /// `3 * NL + 1` sub-bands of the F.3.1 order — exactly the condition
    /// under which [`Quantization::band_quant`] falls back to the (E-5)
    /// derivation for some existing band. The decode stage turns this
    /// into one benign note per codestream.
    pub(crate) fn short_for(&self, levels: u8) -> bool {
        let needed = 3 * usize::from(levels) + 1;
        match &self.style {
            QuantizationStyle::None { exponents } => exponents.len() < needed,
            QuantizationStyle::ScalarDerived { .. } => false,
            QuantizationStyle::ScalarExpounded { steps } => steps.len() < needed,
        }
    }
}

/// Quantization component override, QCC (A.6.5).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Qcc {
    /// Cqcc component index.
    pub component: u16,
    /// Sqcc/SPqcc payload, same shape as QCD.
    pub quant: Quantization,
}

/// Region of interest, RGN (A.6.3). Only Srgn = 0 (implicit ROI, maxshift,
/// Table A.25) is representable; other styles are skipped with a warning.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rgn {
    /// Crgn component index.
    pub component: u16,
    /// SPrgn: binary shift of ROI coefficients above the background
    /// (Table A.26; undone per H.1/H.2).
    pub shift: u8,
}

/// One progression change of a POC marker segment (A.6.6, Table A.32).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PocSegment {
    /// RSpoc: resolution level start (inclusive).
    pub res_start: u8,
    /// CSpoc: component start (inclusive).
    pub comp_start: u16,
    /// LYEpoc: layer end (exclusive; layers always count from zero).
    pub layer_end: u16,
    /// REpoc: resolution level end (exclusive).
    pub res_end: u8,
    /// CEpoc: component end (exclusive; the signalled 0 is decoded as the
    /// full component count by the parser).
    pub comp_end: u16,
    /// Ppoc progression order for this span (Table A.16).
    pub order: ProgressionOrder,
}

/// One PPM marker segment (A.7.4): packed packet headers in the main
/// header. Payloads concatenate in Zppm order; the Nppm length prefixes
/// (which may straddle segment boundaries) split the concatenation into one
/// blob per TILE-PART in codestream appearance order.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Ppm {
    /// Zppm index of this segment.
    pub index: u8,
    /// Raw payload after Zppm (Nppm/Ippm series, possibly partial).
    pub data: Vec<u8>,
}

/// One PPT marker segment (A.7.5): packed packet headers in a tile-part
/// header. Payloads concatenate in (tile-part order, then Zppt) order.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Ppt {
    /// Zppt index of this segment within its tile-part header.
    pub index: u8,
    /// Raw packed packet header bytes.
    pub data: Vec<u8>,
}

/// Start of tile-part, SOT (A.4.2, Table A.5).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sot {
    /// Isot: tile index in raster order, from 0.
    pub tile_index: u16,
    /// Psot: bytes from the first byte of the SOT marker to the end of the
    /// tile-part data; 0 = extends to EOC (last tile-part only). Consumed
    /// by the markers stage when walking tile-parts.
    // Read by the markers stage while walking tile-parts.
    #[allow(dead_code)]
    pub tile_part_length: u32,
    /// TPsot: tile-part index within the tile, from 0.
    pub tile_part_index: u8,
    /// TNsot: declared tile-part count; 0 = not signalled here. A.4.2
    /// allows only the CORRECT count or zero, but real-world streams ship
    /// more parts than declared; extra parts are decoded anyway as a
    /// deliberate compatibility choice, and the decode stage warns once
    /// per codestream.
    pub tile_part_count: u8,
}

/// Everything parsed from the main header (A.3: SOC, SIZ, then functional
/// and pointer marker segments up to the first SOT).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct MainHeader {
    /// SIZ (A.5.1); exactly one.
    pub siz: Siz,
    /// COD (A.6.1); exactly one.
    pub cod: Cod,
    /// Main-header COC overrides (A.6.2), at most one per component.
    pub coc: Vec<Coc>,
    /// QCD (A.6.4); exactly one.
    pub qcd: Quantization,
    /// Main-header QCC overrides (A.6.5), at most one per component.
    pub qcc: Vec<Qcc>,
    /// Main-header RGN segments (A.6.3), at most one per component.
    pub rgn: Vec<Rgn>,
    /// Main-header POC progression changes (A.6.6); empty = none.
    pub poc: Vec<PocSegment>,
    /// PPM segments in Zppm order (A.7.4); empty = headers are in-stream
    /// or in PPT segments.
    pub ppm: Vec<Ppm>,
}

/// Marker segments found in ONE tile-part header (A.4.2..A.4.3). COD/COC/
/// QCD/QCC/RGN/POC are only legal in the TPsot = 0 header (their A.6 usage
/// clauses); PPT may appear in any tile-part header.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct TileOverrides {
    /// Tile COD override (A.6.1 precedence).
    pub cod: Option<Cod>,
    /// Tile COC overrides (A.6.2 precedence).
    pub coc: Vec<Coc>,
    /// Tile QCD override (A.6.4 precedence).
    pub qcd: Option<Quantization>,
    /// Tile QCC overrides (A.6.5 precedence).
    pub qcc: Vec<Qcc>,
    /// Tile RGN overrides (A.6.3 precedence).
    pub rgn: Vec<Rgn>,
    /// Tile POC (A.6.6 precedence: tile-part POC > main POC).
    pub poc: Vec<PocSegment>,
    /// PPT segments of this tile-part header, Zppt order (A.7.5).
    pub ppt: Vec<Ppt>,
}

/// One tile-part: its SOT, its header marker segments, and its bit-stream
/// body (the bytes between SOD and the next SOT/EOC, A.4.3).
#[derive(Debug)]
pub(crate) struct TilePart<'a> {
    /// SOT fields (A.4.2).
    pub sot: Sot,
    /// Marker segments of this tile-part header.
    pub overrides: TileOverrides,
    /// Body bytes. Packets flow across tile-part boundaries, so bodies of
    /// one tile concatenate in decoding order before Tier-2 runs (B.11).
    pub body: &'a [u8],
}

/// A fully scanned codestream: main header plus every tile-part in
/// codestream appearance order (tile-parts of different tiles interleave,
/// A.4.2).
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct Codestream<'a> {
    /// Main header (everything before the first SOT).
    pub main: MainHeader,
    /// Tile-parts in appearance order. Psot = 0 (final tile-part running
    /// to EOC) is resolved to a concrete body slice during the scan.
    pub tile_parts: Vec<TilePart<'a>>,
    /// Soft findings: unknown markers skipped, a missing EOC, etc.,
    /// classified per [`JpxWarning::data_loss`].
    pub warnings: Vec<JpxWarning>,
}

/// Bounds-checked big-endian cursor over untrusted bytes. Every failed
/// read is a `Malformed` naming the structure being parsed; no read ever
/// panics, whatever the input.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn at_end(&self) -> bool {
        self.pos == self.data.len()
    }

    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(truncated(what));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u8(&mut self, what: &str) -> Result<u8> {
        Ok(self.take(1, what)?[0])
    }

    fn u16(&mut self, what: &str) -> Result<u16> {
        let b = self.take(2, what)?;
        Ok(u16::from(b[0]) << 8 | u16::from(b[1]))
    }

    fn u32(&mut self, what: &str) -> Result<u32> {
        let b = self.take(4, what)?;
        Ok(u32::from(b[0]) << 24 | u32::from(b[1]) << 16 | u32::from(b[2]) << 8 | u32::from(b[3]))
    }
}

fn truncated(what: &str) -> JpxError {
    JpxError::Malformed(format!("{what}: truncated"))
}

fn malformed(detail: String) -> JpxError {
    JpxError::Malformed(detail)
}

/// The display name of a known marker, for warnings and errors.
fn marker_name(marker: u16) -> &'static str {
    match marker {
        SOC => "SOC",
        SIZ => "SIZ",
        COD => "COD",
        COC => "COC",
        TLM => "TLM",
        PLM => "PLM",
        PLT => "PLT",
        QCD => "QCD",
        QCC => "QCC",
        RGN => "RGN",
        POC => "POC",
        PPM => "PPM",
        PPT => "PPT",
        CRG => "CRG",
        COM => "COM",
        SOT => "SOT",
        SOP => "SOP",
        EPH => "EPH",
        SOD => "SOD",
        EOC => "EOC",
        _ => "unknown marker",
    }
}

/// Reads a marker segment's Lmar length field (A.1.2: the length counts
/// itself but not the marker) and returns the segment payload.
fn segment_payload<'a>(r: &mut Reader<'a>, what: &str) -> Result<&'a [u8]> {
    let lmar = usize::from(r.u16(what)?);
    if lmar < 2 {
        return Err(malformed(format!(
            "{what}: segment length {lmar} below the 2-byte minimum (A.1.2)"
        )));
    }
    r.take(lmar - 2, what)
}

/// Decodes a Table A.16 progression order value.
fn progression_from(value: u8, what: &str) -> Result<ProgressionOrder> {
    match value {
        0 => Ok(ProgressionOrder::Lrcp),
        1 => Ok(ProgressionOrder::Rlcp),
        2 => Ok(ProgressionOrder::Rpcl),
        3 => Ok(ProgressionOrder::Pcrl),
        4 => Ok(ProgressionOrder::Cprl),
        other => Err(malformed(format!(
            "{what}: reserved progression order {other} (Table A.16)"
        ))),
    }
}

/// Reads a component index: 8 bits when Csiz < 257, 16 bits otherwise
/// (Tables A.22, A.24, A.31, A.32).
fn component_index(r: &mut Reader<'_>, csiz: u16, what: &str) -> Result<u16> {
    if csiz < 257 {
        Ok(u16::from(r.u8(what)?))
    } else {
        r.u16(what)
    }
}

/// Parses the SIZ payload (A.5.1, Table A.9). `limits` bounds the
/// component count before the component list is allocated.
fn parse_siz(payload: &[u8], limits: &DecodeLimits, warnings: &mut Vec<JpxWarning>) -> Result<Siz> {
    let mut r = Reader::new(payload);
    let rsiz = r.u16("SIZ")?;
    if rsiz > 2 {
        // Table A.10 defines 0..=2; anything else is a capability this
        // decoder does not know, kept as a soft finding.
        warnings.push(JpxWarning::note(format!(
            "SIZ: reserved Rsiz capability {rsiz} (Table A.10)"
        )));
    }
    let xsiz = r.u32("SIZ")?;
    let ysiz = r.u32("SIZ")?;
    let xosiz = r.u32("SIZ")?;
    let yosiz = r.u32("SIZ")?;
    let xtsiz = r.u32("SIZ")?;
    let ytsiz = r.u32("SIZ")?;
    let xtosiz = r.u32("SIZ")?;
    let ytosiz = r.u32("SIZ")?;
    if xsiz == 0 || ysiz == 0 || xtsiz == 0 || ytsiz == 0 {
        return Err(malformed(
            "SIZ: zero grid or tile dimension (Table A.9)".into(),
        ));
    }
    if xosiz >= xsiz || yosiz >= ysiz {
        return Err(malformed(
            "SIZ: image offset leaves an empty image area (A.5.1)".into(),
        ));
    }
    if xtosiz > xosiz || ytosiz > yosiz {
        return Err(malformed(
            "SIZ: tile offset beyond the image offset (Equation (B-3))".into(),
        ));
    }
    if u64::from(xtosiz) + u64::from(xtsiz) <= u64::from(xosiz)
        || u64::from(ytosiz) + u64::from(ytsiz) <= u64::from(yosiz)
    {
        return Err(malformed(
            "SIZ: the first tile misses the image area (Equation (B-4))".into(),
        ));
    }
    let csiz = r.u16("SIZ")?;
    if csiz == 0 || csiz > 16384 {
        return Err(malformed(format!(
            "SIZ: Csiz {csiz} outside 1..=16384 (Table A.9)"
        )));
    }
    if u64::from(csiz) > u64::from(limits.max_components) {
        return Err(JpxError::LimitExceeded {
            what: "max_components",
            actual: u64::from(csiz),
            limit: u64::from(limits.max_components),
        });
    }
    if r.remaining() != 3 * usize::from(csiz) {
        return Err(malformed(format!(
            "SIZ: Lsiz disagrees with Csiz {csiz} (Equation (A-1))"
        )));
    }
    let mut components = Vec::with_capacity(usize::from(csiz));
    for _ in 0..csiz {
        let ssiz = r.u8("SIZ")?;
        let xrsiz = r.u8("SIZ")?;
        let yrsiz = r.u8("SIZ")?;
        // Table A.11: bits 0-6 hold depth - 1 (0..=37), bit 7 the sign.
        let depth_field = ssiz & 127;
        if depth_field > 37 {
            return Err(malformed(format!(
                "SIZ: Ssiz precision {depth_field} beyond 38 bits (Table A.11)"
            )));
        }
        if xrsiz == 0 || yrsiz == 0 {
            return Err(malformed("SIZ: zero XRsiz or YRsiz (Table A.9)".into()));
        }
        components.push(SizComponent {
            depth: depth_field + 1,
            signed: ssiz & 128 != 0,
            xrsiz,
            yrsiz,
        });
    }
    Ok(Siz {
        rsiz,
        xsiz,
        ysiz,
        xosiz,
        yosiz,
        xtsiz,
        ytsiz,
        xtosiz,
        ytosiz,
        components,
    })
}

/// Parses the SPcod/SPcoc tail (Table A.15), including the Table A.21
/// precinct list when the Scod/Scoc precinct bit was set.
fn parse_coding_style(
    r: &mut Reader<'_>,
    precincts_signalled: bool,
    what: &str,
) -> Result<CodingStyle> {
    let levels = r.u8(what)?;
    if levels > 32 {
        return Err(malformed(format!(
            "{what}: {levels} decomposition levels above 32 (Table A.15)"
        )));
    }
    let xcb0 = r.u8(what)?;
    let ycb0 = r.u8(what)?;
    if xcb0 > 8 || ycb0 > 8 {
        return Err(malformed(format!(
            "{what}: code-block exponent offset above 8 (Table A.18)"
        )));
    }
    // Table A.18 signals xcb - 2 / ycb - 2 and caps the code-block area.
    let width_exp = xcb0 + 2;
    let height_exp = ycb0 + 2;
    if width_exp + height_exp > 12 {
        return Err(malformed(format!(
            "{what}: code-block area above 4096 samples (Table A.18)"
        )));
    }
    let code_block_style = r.u8(what)?;
    let wavelet = match r.u8(what)? {
        0 => WaveletKind::Irreversible97,
        1 => WaveletKind::Reversible53,
        other => {
            return Err(malformed(format!(
                "{what}: reserved transformation {other} (Table A.20)"
            )));
        }
    };
    let mut precincts = Vec::new();
    if precincts_signalled {
        // One byte per resolution level, NL LL first (Table A.15).
        for level in 0..=levels {
            let byte = r.u8(what)?;
            let ppx = byte & 15;
            let ppy = byte >> 4;
            if level > 0 && (ppx == 0 || ppy == 0) {
                return Err(malformed(format!(
                    "{what}: zero precinct exponent above resolution 0 (Table A.21)"
                )));
            }
            precincts.push(PrecinctExponents { ppx, ppy });
        }
    }
    Ok(CodingStyle {
        decomposition_levels: levels,
        code_block_width_exp: width_exp,
        code_block_height_exp: height_exp,
        code_block_style,
        wavelet,
        precincts,
    })
}

/// Parses a COD payload (A.6.1, Figures A.8/A.9).
fn parse_cod(payload: &[u8]) -> Result<Cod> {
    let mut r = Reader::new(payload);
    // Scod (Table A.13): bit 0 user precincts, bit 1 SOP, bit 2 EPH;
    // higher bits are reserved-zero and ignored (A.1.3).
    let scod = r.u8("COD")?;
    let progression = progression_from(r.u8("COD")?, "COD")?;
    let layers = r.u16("COD")?;
    if layers == 0 {
        return Err(malformed("COD: zero layers (Table A.14)".into()));
    }
    let mct = r.u8("COD")?;
    if mct > 1 {
        return Err(malformed(format!(
            "COD: reserved component transformation {mct} (Table A.17)"
        )));
    }
    let style = parse_coding_style(&mut r, scod & 1 != 0, "COD")?;
    if !r.at_end() {
        return Err(malformed(format!(
            "COD: {} bytes after SPcod (Equation (A-2))",
            r.remaining()
        )));
    }
    Ok(Cod {
        sop_markers: scod & 2 != 0,
        eph_markers: scod & 4 != 0,
        progression,
        layers,
        mct,
        style,
    })
}

/// Parses a COC payload (A.6.2, Figures A.10/A.11).
fn parse_coc(payload: &[u8], csiz: u16) -> Result<Coc> {
    let mut r = Reader::new(payload);
    let component = component_index(&mut r, csiz, "COC")?;
    // Scoc (Table A.23): only bit 0 (user precincts) is defined.
    let scoc = r.u8("COC")?;
    let style = parse_coding_style(&mut r, scoc & 1 != 0, "COC")?;
    if !r.at_end() {
        return Err(malformed(format!(
            "COC: {} bytes after SPcoc (Equation (A-3))",
            r.remaining()
        )));
    }
    Ok(Coc { component, style })
}

/// Parses the Sqcd/Sqcc byte and step-size list shared by QCD and QCC
/// (Tables A.28-A.30), consuming the reader to its end.
fn parse_quantization(r: &mut Reader<'_>, what: &str) -> Result<Quantization> {
    let sq = r.u8(what)?;
    // Table A.28: bits 0-4 select the style, bits 5-7 count guard bits.
    let guard_bits = sq >> 5;
    let style = match sq & 31 {
        0 => {
            if r.at_end() {
                return Err(malformed(format!(
                    "{what}: no reversible step sizes (Equation (A-4))"
                )));
            }
            let mut exponents = Vec::with_capacity(r.remaining());
            while !r.at_end() {
                // Table A.29: the exponent lives in the five MSBs.
                exponents.push(r.u8(what)? >> 3);
            }
            QuantizationStyle::None { exponents }
        }
        1 => {
            // Table A.30: exponent in the top five bits, mantissa below.
            let word = r.u16(what)?;
            QuantizationStyle::ScalarDerived {
                exponent: (word >> 11) as u8,
                mantissa: word & 2047,
            }
        }
        2 => {
            if r.remaining() == 0 || !r.remaining().is_multiple_of(2) {
                return Err(malformed(format!(
                    "{what}: ragged expounded step-size list (Equation (A-4))"
                )));
            }
            let mut steps = Vec::with_capacity(r.remaining() / 2);
            while !r.at_end() {
                let word = r.u16(what)?;
                steps.push(QuantStep {
                    exponent: (word >> 11) as u8,
                    mantissa: word & 2047,
                });
            }
            QuantizationStyle::ScalarExpounded { steps }
        }
        other => {
            return Err(malformed(format!(
                "{what}: reserved quantization style {other} (Table A.28)"
            )));
        }
    };
    if !r.at_end() {
        return Err(malformed(format!(
            "{what}: trailing bytes after the step size (Equation (A-4))"
        )));
    }
    Ok(Quantization { guard_bits, style })
}

/// Parses a QCD payload (A.6.4, Figure A.13).
fn parse_qcd(payload: &[u8]) -> Result<Quantization> {
    parse_quantization(&mut Reader::new(payload), "QCD")
}

/// Parses a QCC payload (A.6.5, Figure A.14).
fn parse_qcc(payload: &[u8], csiz: u16) -> Result<Qcc> {
    let mut r = Reader::new(payload);
    let component = component_index(&mut r, csiz, "QCC")?;
    let quant = parse_quantization(&mut r, "QCC")?;
    Ok(Qcc { component, quant })
}

/// Parses an RGN payload (A.6.3, Figure A.12). Reserved ROI styles
/// (Table A.25 defines only 0, maxshift) are skipped with a warning.
fn parse_rgn(payload: &[u8], csiz: u16, warnings: &mut Vec<JpxWarning>) -> Result<Option<Rgn>> {
    let mut r = Reader::new(payload);
    let component = component_index(&mut r, csiz, "RGN")?;
    let srgn = r.u8("RGN")?;
    let shift = r.u8("RGN")?;
    if !r.at_end() {
        return Err(malformed(format!(
            "RGN: {} bytes after SPrgn (Table A.24)",
            r.remaining()
        )));
    }
    if srgn != 0 {
        // An ROI scaling of unknown semantics stays applied to the
        // coefficients: the affected pixels cannot be trusted.
        warnings.push(JpxWarning::loss(format!(
            "RGN: reserved ROI style {srgn} skipped (Table A.25)"
        )));
        return Ok(None);
    }
    Ok(Some(Rgn { component, shift }))
}

/// Parses a POC payload (A.6.6, Table A.32): each progression change is 7
/// bytes when Csiz < 257 and 9 bytes otherwise (Equation (A-6)).
fn parse_poc(payload: &[u8], csiz: u16, component_count: u16) -> Result<Vec<PocSegment>> {
    let entry_len = if csiz < 257 { 7 } else { 9 };
    if payload.is_empty() || !payload.len().is_multiple_of(entry_len) {
        return Err(malformed(
            "POC: Lpoc holds no whole progression change (Equation (A-6))".into(),
        ));
    }
    let mut r = Reader::new(payload);
    let mut segments = Vec::with_capacity(payload.len() / entry_len);
    while !r.at_end() {
        let res_start = r.u8("POC")?;
        if res_start > 32 {
            return Err(malformed(format!(
                "POC: RSpoc {res_start} above 32 (Table A.32)"
            )));
        }
        let comp_start = component_index(&mut r, csiz, "POC")?;
        let layer_end = r.u16("POC")?;
        if layer_end == 0 {
            return Err(malformed("POC: zero LYEpoc (Table A.32)".into()));
        }
        let res_end = r.u8("POC")?;
        if res_end <= res_start || res_end > 33 {
            return Err(malformed(format!(
                "POC: REpoc {res_end} outside RSpoc+1..=33 (Table A.32)"
            )));
        }
        let signalled_comp_end = component_index(&mut r, csiz, "POC")?;
        // Table A.32: a signalled 0 means the full component count.
        let comp_end = if signalled_comp_end == 0 {
            component_count
        } else {
            signalled_comp_end
        };
        if comp_end <= comp_start {
            return Err(malformed(format!(
                "POC: CEpoc {comp_end} not above CSpoc {comp_start} (Table A.32)"
            )));
        }
        let order = progression_from(r.u8("POC")?, "POC")?;
        segments.push(PocSegment {
            res_start,
            comp_start,
            layer_end,
            res_end,
            comp_end,
            order,
        });
    }
    Ok(segments)
}

/// Parses a PPM payload (A.7.4, Figure A.20): Zppm then raw series bytes.
fn parse_ppm(payload: &[u8]) -> Result<Ppm> {
    match payload.split_first() {
        Some((&index, data)) => Ok(Ppm {
            index,
            data: data.to_vec(),
        }),
        None => Err(malformed("PPM: missing Zppm (Table A.38)".into())),
    }
}

/// Parses a PPT payload (A.7.5, Figure A.21): Zppt then packed headers.
fn parse_ppt(payload: &[u8]) -> Result<Ppt> {
    match payload.split_first() {
        Some((&index, data)) => Ok(Ppt {
            index,
            data: data.to_vec(),
        }),
        None => Err(malformed("PPT: missing Zppt (Table A.39)".into())),
    }
}

/// Parses a SOT payload (A.4.2, Table A.5).
fn parse_sot(payload: &[u8]) -> Result<Sot> {
    if payload.len() != 8 {
        return Err(malformed(format!(
            "SOT: Lsot {} instead of the fixed 10 (Table A.5)",
            payload.len() + 2
        )));
    }
    let mut r = Reader::new(payload);
    let tile_index = r.u16("SOT")?;
    if tile_index == 65535 {
        return Err(malformed("SOT: Isot 65535 above 65534 (Table A.5)".into()));
    }
    let tile_part_length = r.u32("SOT")?;
    if tile_part_length != 0 && tile_part_length < 14 {
        return Err(malformed(format!(
            "SOT: Psot {tile_part_length} inside the forbidden 1..=13 (Table A.5)"
        )));
    }
    let tile_part_index = r.u8("SOT")?;
    if tile_part_index == 255 {
        return Err(malformed("SOT: TPsot 255 above 254 (Table A.5)".into()));
    }
    let tile_part_count = r.u8("SOT")?;
    Ok(Sot {
        tile_index,
        tile_part_length,
        tile_part_index,
        tile_part_count,
    })
}

/// Records a per-component override list entry: a component beyond Csiz is
/// dropped with a warning, and a duplicate component warns and is replaced
/// (the A.6 usage clauses allow at most one per component).
fn record_override<T>(
    list: &mut Vec<T>,
    parsed: T,
    key: impl Fn(&T) -> u16,
    csiz: u16,
    kind: &str,
    context: &str,
    warnings: &mut Vec<JpxWarning>,
) {
    let component = key(&parsed);
    if component >= csiz {
        warnings.push(JpxWarning::note(format!(
            "{context}: {kind} for component {component} beyond Csiz = {csiz} dropped"
        )));
        return;
    }
    if let Some(existing) = list.iter_mut().find(|entry| key(entry) == component) {
        warnings.push(JpxWarning::note(format!(
            "{context}: duplicate {kind} for component {component}; the last wins"
        )));
        *existing = parsed;
    } else {
        list.push(parsed);
    }
}

/// Scans one tile-part header from just after its SOT segment to just
/// after SOD (Figures A.4/A.5). Errors bubble to the caller, which
/// degrades them to a warning plus a truncated tail (leniency doctrine).
fn scan_tile_part_header(
    r: &mut Reader<'_>,
    csiz: u16,
    sot: Sot,
    warnings: &mut Vec<JpxWarning>,
) -> Result<TileOverrides> {
    let mut overrides = TileOverrides::default();
    let context = format!("tile {} part {}", sot.tile_index, sot.tile_part_index);
    // A.6.1..A.6.5: coding and quantization markers belong in the
    // TPsot = 0 header only; later appearances warn but are honoured.
    let late_check = |kind: &str, warnings: &mut Vec<JpxWarning>| {
        if sot.tile_part_index > 0 {
            warnings.push(JpxWarning::note(format!(
                "{context}: {kind} in a non-first tile-part header honoured leniently (A.6)"
            )));
        }
    };
    loop {
        let marker = r.u16("tile-part header")?;
        match marker {
            SOD => return Ok(overrides),
            COD => {
                late_check("COD", warnings);
                let parsed = parse_cod(segment_payload(r, "COD")?)?;
                if overrides.cod.replace(parsed).is_some() {
                    warnings.push(JpxWarning::note(format!(
                        "{context}: duplicate COD; the last wins (A.6.1)"
                    )));
                }
            }
            COC => {
                late_check("COC", warnings);
                let parsed = parse_coc(segment_payload(r, "COC")?, csiz)?;
                record_override(
                    &mut overrides.coc,
                    parsed,
                    |c| c.component,
                    csiz,
                    "COC",
                    &context,
                    warnings,
                );
            }
            QCD => {
                late_check("QCD", warnings);
                let parsed = parse_qcd(segment_payload(r, "QCD")?)?;
                if overrides.qcd.replace(parsed).is_some() {
                    warnings.push(JpxWarning::note(format!(
                        "{context}: duplicate QCD; the last wins (A.6.4)"
                    )));
                }
            }
            QCC => {
                late_check("QCC", warnings);
                let parsed = parse_qcc(segment_payload(r, "QCC")?, csiz)?;
                record_override(
                    &mut overrides.qcc,
                    parsed,
                    |q| q.component,
                    csiz,
                    "QCC",
                    &context,
                    warnings,
                );
            }
            RGN => {
                late_check("RGN", warnings);
                if let Some(parsed) = parse_rgn(segment_payload(r, "RGN")?, csiz, warnings)? {
                    record_override(
                        &mut overrides.rgn,
                        parsed,
                        |x| x.component,
                        csiz,
                        "RGN",
                        &context,
                        warnings,
                    );
                }
            }
            // A POC may appear in any tile-part header as long as it
            // precedes the packets it governs (A.6.6).
            POC => overrides
                .poc
                .extend(parse_poc(segment_payload(r, "POC")?, csiz, csiz)?),
            PPT => overrides.ppt.push(parse_ppt(segment_payload(r, "PPT")?)?),
            PLT => {
                // Parse-and-skip (A.7.3); the packet stage never needs it.
                segment_payload(r, "PLT")?;
            }
            COM => {
                // Parse-and-skip (A.9.2).
                segment_payload(r, "COM")?;
            }
            TLM | PLM | CRG | PPM | SOP => {
                warnings.push(JpxWarning::note(format!(
                    "{context}: {} not allowed in a tile-part header skipped (Table A.2)",
                    marker_name(marker)
                )));
                segment_payload(r, marker_name(marker))?;
            }
            EPH => {
                warnings.push(JpxWarning::note(format!(
                    "{context}: stray EPH skipped (A.8.2)"
                )));
            }
            RESERVED_NO_SEGMENT_FIRST..=RESERVED_NO_SEGMENT_LAST => {
                // A.1.3: reserved markers without parameters are skipped.
            }
            SOT | SOC | EOC | SIZ => {
                return Err(malformed(format!(
                    "{context}: {} before SOD (A.4.3)",
                    marker_name(marker)
                )));
            }
            other => {
                if other >> 8 != 255 {
                    return Err(malformed(format!(
                        "{context}: byte-aligned garbage {other} where a marker was expected (A.1.2)"
                    )));
                }
                warnings.push(JpxWarning::note(format!(
                    "{context}: unknown marker {other} skipped"
                )));
                segment_payload(r, "unknown segment")?;
            }
        }
    }
}

/// Scans a raw codestream: SOC (A.4.1), main header marker segments, then
/// every tile-part header/body up to EOC (A.4.4).
///
/// Header-level problems (missing/duplicated SIZ/COD/QCD, truncated marker
/// segments before the first tile) are hard `Malformed` errors; from the
/// first tile-part on, structural surprises degrade to warnings and a
/// truncated tail (leniency doctrine, crate docs). `limits` bounds every
/// count read from the input before it sizes an allocation.
pub(crate) fn parse_codestream<'a>(
    data: &'a [u8],
    limits: &DecodeLimits,
) -> Result<Codestream<'a>> {
    let mut warnings = Vec::new();
    let mut r = Reader::new(data);
    if r.u16("codestream")? != SOC {
        return Err(malformed("codestream: missing SOC (A.4.1)".into()));
    }
    if r.u16("main header")? != SIZ {
        return Err(malformed(
            "main header: SIZ must immediately follow SOC (A.5.1)".into(),
        ));
    }
    let siz = parse_siz(segment_payload(&mut r, "SIZ")?, limits, &mut warnings)?;
    let csiz = siz.components.len() as u16;

    let mut cod = None;
    let mut qcd = None;
    let mut coc: Vec<Coc> = Vec::new();
    let mut qcc: Vec<Qcc> = Vec::new();
    let mut rgn: Vec<Rgn> = Vec::new();
    let mut poc: Vec<PocSegment> = Vec::new();
    let mut ppm: Vec<Ppm> = Vec::new();

    // Main header: functional and pointer marker segments up to the first
    // SOT (Figure A.3). Problems in here are hard errors.
    let first_sot;
    let first_sot_start;
    loop {
        let marker_start = r.pos;
        let marker = r.u16("main header")?;
        match marker {
            SOT => {
                first_sot_start = marker_start;
                first_sot = parse_sot(segment_payload(&mut r, "SOT")?)?;
                break;
            }
            SIZ | SOC => {
                return Err(malformed(format!(
                    "main header: duplicate {} (A.5.1)",
                    marker_name(marker)
                )));
            }
            COD => {
                let parsed = parse_cod(segment_payload(&mut r, "COD")?)?;
                if cod.replace(parsed).is_some() {
                    return Err(malformed("main header: duplicate COD (A.6.1)".into()));
                }
            }
            COC => {
                let parsed = parse_coc(segment_payload(&mut r, "COC")?, csiz)?;
                record_override(
                    &mut coc,
                    parsed,
                    |c| c.component,
                    csiz,
                    "COC",
                    "main header",
                    &mut warnings,
                );
            }
            QCD => {
                let parsed = parse_qcd(segment_payload(&mut r, "QCD")?)?;
                if qcd.replace(parsed).is_some() {
                    return Err(malformed("main header: duplicate QCD (A.6.4)".into()));
                }
            }
            QCC => {
                let parsed = parse_qcc(segment_payload(&mut r, "QCC")?, csiz)?;
                record_override(
                    &mut qcc,
                    parsed,
                    |q| q.component,
                    csiz,
                    "QCC",
                    "main header",
                    &mut warnings,
                );
            }
            RGN => {
                if let Some(parsed) =
                    parse_rgn(segment_payload(&mut r, "RGN")?, csiz, &mut warnings)?
                {
                    record_override(
                        &mut rgn,
                        parsed,
                        |x| x.component,
                        csiz,
                        "RGN",
                        "main header",
                        &mut warnings,
                    );
                }
            }
            POC => {
                // A.6.6: at most one POC per header; extras concatenate
                // leniently with a warning.
                if !poc.is_empty() {
                    warnings.push(JpxWarning::note("main header: more than one POC (A.6.6)"));
                }
                poc.extend(parse_poc(segment_payload(&mut r, "POC")?, csiz, csiz)?);
            }
            PPM => ppm.push(parse_ppm(segment_payload(&mut r, "PPM")?)?),
            TLM | PLM | CRG | COM => {
                // Parse-and-skip: TLM (A.7.1), PLM (A.7.2), CRG (A.9.1),
                // COM (A.9.2) carry nothing the decoder needs.
                segment_payload(&mut r, marker_name(marker))?;
            }
            PLT | PPT | SOP => {
                warnings.push(JpxWarning::note(format!(
                    "main header: {} not allowed here skipped (Table A.2)",
                    marker_name(marker)
                )));
                segment_payload(&mut r, marker_name(marker))?;
            }
            EPH => {
                warnings.push(JpxWarning::note("main header: stray EPH skipped (A.8.2)"));
            }
            SOD | EOC => {
                return Err(malformed(format!(
                    "main header: {} before any tile-part (A.4)",
                    marker_name(marker)
                )));
            }
            RESERVED_NO_SEGMENT_FIRST..=RESERVED_NO_SEGMENT_LAST => {
                // A.1.3: reserved markers without parameters are skipped.
            }
            other => {
                if other >> 8 != 255 {
                    return Err(malformed(format!(
                        "main header: byte-aligned garbage {other} where a marker was expected (A.1.2)"
                    )));
                }
                warnings.push(JpxWarning::note(format!(
                    "main header: unknown marker {other} skipped"
                )));
                segment_payload(&mut r, "unknown segment")?;
            }
        }
    }
    let cod = cod.ok_or_else(|| malformed("main header: missing COD (A.6.1)".into()))?;
    let qcd = qcd.ok_or_else(|| malformed("main header: missing QCD (A.6.4)".into()))?;
    let main = MainHeader {
        siz,
        cod,
        coc,
        qcd,
        qcc,
        rgn,
        poc,
        ppm,
    };

    // Tile territory: from the first SOT on, structural surprises degrade
    // to warnings and a truncated tail (leniency doctrine, crate docs).
    let mut tile_parts: Vec<TilePart<'a>> = Vec::new();
    let mut sot = first_sot;
    let mut sot_start = first_sot_start;
    loop {
        // A.4.2 makes TNsot normative (the correct count or zero), but
        // measured real-world streams declare fewer tile-parts than they
        // ship; a surplus index parses and decodes anyway (compatibility
        // choice). The decode stage summarizes the violation in one
        // warning per codestream.
        let overrides = match scan_tile_part_header(&mut r, csiz, sot, &mut warnings) {
            Ok(overrides) => overrides,
            Err(e) => {
                // The rest of the codestream is dropped: pixel loss.
                warnings.push(JpxWarning::loss(format!(
                    "tile-part header abandoned, tail truncated: {e}"
                )));
                break;
            }
        };
        let body_start = r.pos;
        let psot = sot.tile_part_length;
        let body_end = if psot == 0 {
            // A.4.2: Psot = 0 marks the final tile-part, whose data runs
            // to the EOC marker closing the codestream (A.4.4).
            if data.len() >= body_start + 2 && data[data.len() - 2..] == EOC.to_be_bytes() {
                data.len() - 2
            } else {
                warnings.push(JpxWarning::note(
                    "codestream: missing EOC after the final tile-part (A.4.4)",
                ));
                data.len()
            }
        } else {
            // Psot spans from the first byte of the SOT marker to the end
            // of the tile-part data (Figure A.16).
            match sot_start.checked_add(psot as usize) {
                Some(end) if end >= body_start && end <= data.len() => end,
                Some(end) if end > data.len() => {
                    // Truncation: whatever the overrun swallowed is gone.
                    warnings.push(JpxWarning::loss(format!(
                        "tile {}: Psot {psot} overruns the codestream; body truncated",
                        sot.tile_index
                    )));
                    data.len()
                }
                _ => {
                    // Keeping the tail as body swallows every later
                    // tile-part: their tiles lose data.
                    warnings.push(JpxWarning::loss(format!(
                        "tile {}: Psot {psot} ends before its own SOD; tail kept as body",
                        sot.tile_index
                    )));
                    data.len()
                }
            }
        };
        tile_parts.push(TilePart {
            sot,
            overrides,
            body: &data[body_start..body_end],
        });
        if psot == 0 {
            break;
        }
        r.pos = body_end;
        if r.at_end() {
            warnings.push(JpxWarning::note("codestream: missing EOC (A.4.4)"));
            break;
        }
        let marker_start = r.pos;
        let marker = match r.u16("codestream") {
            Ok(marker) => marker,
            Err(_) => {
                warnings.push(JpxWarning::note(
                    "codestream: lone trailing byte after a tile-part",
                ));
                break;
            }
        };
        match marker {
            EOC => {
                if !r.at_end() {
                    warnings.push(JpxWarning::note(format!(
                        "codestream: {} bytes after EOC ignored (A.4.4)",
                        r.remaining()
                    )));
                }
                break;
            }
            SOT => match segment_payload(&mut r, "SOT").and_then(parse_sot) {
                Ok(next) => {
                    sot = next;
                    sot_start = marker_start;
                }
                Err(e) => {
                    // The rest of the codestream is dropped: pixel loss.
                    warnings.push(JpxWarning::loss(format!(
                        "tile-part abandoned, tail truncated: {e}"
                    )));
                    break;
                }
            },
            other => {
                // The rest of the codestream is dropped: pixel loss.
                warnings.push(JpxWarning::loss(format!(
                    "codestream: expected SOT or EOC, found {other}; tail truncated"
                )));
                break;
            }
        }
    }
    Ok(Codestream {
        main,
        tile_parts,
        warnings,
    })
}

/// Splits the PPM payload concatenation into one packed-header blob per
/// tile-part, in codestream appearance order (A.7.4: the Nppm/Ippm series
/// continues across PPM segments; the k-th entry belongs to the k-th
/// tile-part of the codestream).
pub(crate) fn split_packed_headers(
    segments: &[Ppm],
    tile_part_count: usize,
) -> Result<Vec<Vec<u8>>> {
    // A.7.4: the Nppm/Ippm series continues across PPM segments in order
    // of increasing Zppm, and a length prefix may straddle a segment
    // boundary, so join the payloads before reading any prefix.
    let mut ordered: Vec<&Ppm> = segments.iter().collect();
    ordered.sort_by_key(|segment| segment.index);
    let total = ordered.iter().map(|segment| segment.data.len()).sum();
    let mut series = Vec::with_capacity(total);
    for segment in &ordered {
        series.extend_from_slice(&segment.data);
    }
    // The k-th (Nppm, Ippm run) entry belongs to the k-th tile-part in
    // codestream appearance order. Entries beyond `tile_part_count` are
    // left unread: a truncated codestream can carry fewer tile-parts than
    // the main header described.
    let mut r = Reader::new(&series);
    let mut blobs = Vec::with_capacity(tile_part_count);
    for _ in 0..tile_part_count {
        let n = r.u32("PPM series")? as usize;
        blobs.push(r.take(n, "PPM series")?.to_vec());
    }
    Ok(blobs)
}

/// Merges the tile-part header overrides of ONE tile, in tile-part order:
/// coding/quantization markers are taken from the TPsot = 0 header (their
/// A.6 usage rules); markers found in later parts produce a warning and are
/// honoured leniently rather than rejected.
pub(crate) fn merge_tile_overrides(parts: &[&TilePart<'_>]) -> Result<TileOverrides> {
    let mut merged = TileOverrides::default();
    for part in parts {
        let overrides = &part.overrides;
        // COD/QCD and the per-component overrides belong to the TPsot = 0
        // header (A.6 usage clauses), so the first occurrence in tile-part
        // order wins; the scan already warned about late appearances,
        // which are honoured here only when the earlier parts had none.
        if merged.cod.is_none() {
            merged.cod = overrides.cod.clone();
        }
        if merged.qcd.is_none() {
            merged.qcd = overrides.qcd.clone();
        }
        for coc in &overrides.coc {
            if merged.coc.iter().all(|c| c.component != coc.component) {
                merged.coc.push(coc.clone());
            }
        }
        for qcc in &overrides.qcc {
            if merged.qcc.iter().all(|q| q.component != qcc.component) {
                merged.qcc.push(qcc.clone());
            }
        }
        for rgn in &overrides.rgn {
            if merged.rgn.iter().all(|x| x.component != rgn.component) {
                merged.rgn.push(*rgn);
            }
        }
        // A.6.6: each progression change is described once, in the header
        // preceding its packets; concatenate in tile-part order.
        merged.poc.extend(overrides.poc.iter().copied());
        // A.7.5: PPT payloads concatenate in Zppt order within a header,
        // then in tile-part order across headers.
        let mut ppt: Vec<&Ppt> = overrides.ppt.iter().collect();
        ppt.sort_by_key(|segment| segment.index);
        merged.ppt.extend(ppt.into_iter().cloned());
    }
    Ok(merged)
}

/// Tile-wide coding parameters after precedence resolution.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct TileCoding {
    /// Progression order before any POC takes over (A.6.1/A.6.6).
    pub progression: ProgressionOrder,
    /// Layer count (SGcod).
    pub layers: u16,
    /// Multiple component transformation flag (Table A.17).
    pub mct: u8,
    /// SOP marker segments possible in this tile's bit stream (Table A.13).
    pub sop_markers: bool,
    /// EPH markers terminate packet headers in this tile (Table A.13).
    pub eph_markers: bool,
    /// Effective POC chain: tile-part POC > main POC > COD progression
    /// (A.6.6 precedence). Empty = single progression from `progression`.
    pub poc: Vec<PocSegment>,
}

/// Resolves the tile-wide coding parameters for one tile (A.6.1/A.6.6
/// precedence: tile-part COD > main COD; tile-part POC > main POC).
pub(crate) fn resolve_tile_coding(main: &MainHeader, tile: &TileOverrides) -> Result<TileCoding> {
    // A.6.1: tile-part COD > main COD for everything SGcod carries.
    let cod = tile.cod.as_ref().unwrap_or(&main.cod);
    // A.6.6: tile-part POC > main POC (never concatenated across levels).
    let poc = if tile.poc.is_empty() {
        main.poc.clone()
    } else {
        tile.poc.clone()
    };
    Ok(TileCoding {
        progression: cod.progression,
        layers: cod.layers,
        mct: cod.mct,
        sop_markers: cod.sop_markers,
        eph_markers: cod.eph_markers,
        poc,
    })
}

/// Coding parameters of one component of one tile after full precedence
/// resolution — the markers → packet seam.
// Constructed by the markers stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct ComponentCoding {
    /// SPcod/SPcoc parameters (A.6.1/A.6.2 precedence:
    /// tile COC > tile COD > main COC > main COD).
    pub style: CodingStyle,
    /// Quantization (A.6.4/A.6.5 precedence:
    /// tile QCC > tile QCD > main QCC > main QCD).
    pub quant: Quantization,
    /// RGN maxshift for this component, if signalled (A.6.3 precedence:
    /// tile RGN > main RGN).
    pub roi_shift: Option<u8>,
}

/// Resolves one component's coding parameters for one tile.
pub(crate) fn resolve_component_coding(
    main: &MainHeader,
    tile: &TileOverrides,
    component: u16,
) -> Result<ComponentCoding> {
    fn coc_style(list: &[Coc], component: u16) -> Option<&CodingStyle> {
        list.iter()
            .find(|c| c.component == component)
            .map(|c| &c.style)
    }
    fn qcc_quant(list: &[Qcc], component: u16) -> Option<&Quantization> {
        list.iter()
            .find(|q| q.component == component)
            .map(|q| &q.quant)
    }
    fn rgn_shift(list: &[Rgn], component: u16) -> Option<u8> {
        list.iter()
            .find(|r| r.component == component)
            .map(|r| r.shift)
    }
    // A.6.1/A.6.2: tile COC > tile COD > main COC > main COD.
    let style = coc_style(&tile.coc, component)
        .or_else(|| tile.cod.as_ref().map(|cod| &cod.style))
        .or_else(|| coc_style(&main.coc, component))
        .unwrap_or(&main.cod.style)
        .clone();
    // A.6.4/A.6.5: tile QCC > tile QCD > main QCC > main QCD.
    let quant = qcc_quant(&tile.qcc, component)
        .or(tile.qcd.as_ref())
        .or_else(|| qcc_quant(&main.qcc, component))
        .unwrap_or(&main.qcd)
        .clone();
    // A.6.3: a tile RGN overrides the main RGN for its component.
    let roi_shift = rgn_shift(&tile.rgn, component).or_else(|| rgn_shift(&main.rgn, component));
    Ok(ComponentCoding {
        style,
        quant,
        roi_shift,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- byte-stream builders ------------------------------------------

    /// A marker segment: marker code, Lmar = payload + 2 (A.1.2), payload.
    fn seg(marker: u16, payload: &[u8]) -> Vec<u8> {
        let mut v = marker.to_be_bytes().to_vec();
        let lmar = u16::try_from(payload.len() + 2).unwrap();
        v.extend(lmar.to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// SIZ payload (Table A.9): Rsiz 0, a 16 x 16 grid with zero offsets,
    /// 16 x 16 tiles with zero offsets, `csiz` components each Ssiz = 7
    /// (8-bit unsigned, Table A.11), XRsiz = YRsiz = 1. Payload length is
    /// Lsiz - 2 = 36 + 3 * Csiz (Equation (A-1)).
    fn tiny_siz_payload(csiz: u16) -> Vec<u8> {
        let mut v = 0u16.to_be_bytes().to_vec();
        for value in [16u32, 16, 0, 0, 16, 16, 0, 0] {
            v.extend(value.to_be_bytes());
        }
        v.extend(csiz.to_be_bytes());
        for _ in 0..csiz {
            v.extend([7, 1, 1]);
        }
        v
    }

    /// COD payload (Figure A.9): Scod 0, LRCP, 1 layer, no MCT, NL = 1,
    /// code-block 16 x 16 (signalled 2 = xcb - 2, Table A.18), style 0,
    /// 5-3 wavelet. Lcod = 12 (Equation (A-2)) so the payload is 10 bytes.
    fn tiny_cod_payload() -> Vec<u8> {
        vec![0, 0, 0, 1, 0, 1, 2, 2, 0, 1]
    }

    /// QCD payload, scalar derived (Table A.28): Sqcd = 33 = 32 + 1, guard
    /// bits 33 >> 5 = 1, style 33 & 31 = 1; one Table A.30 word
    /// 40 * 256 + 100 = 10340 = 5 * 2048 + 100, so exponent 5, mantissa 100.
    fn derived_qcd_payload() -> Vec<u8> {
        vec![33, 40, 100]
    }

    /// A full tile-part: SOT segment (Figure A.6), SOD, body bytes.
    fn tile_part(isot: u16, psot: u32, tpsot: u8, tnsot: u8, body: &[u8]) -> Vec<u8> {
        let mut payload = isot.to_be_bytes().to_vec();
        payload.extend(psot.to_be_bytes());
        payload.extend([tpsot, tnsot]);
        let mut v = seg(SOT, &payload);
        v.extend(SOD.to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    /// SOC + SIZ + COD + QCD: the smallest legal main header (Figure A.3).
    fn tiny_main_header() -> Vec<u8> {
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v
    }

    fn parse(data: &[u8]) -> Result<Codestream<'_>> {
        parse_codestream(data, &DecodeLimits::default())
    }

    // ---- fixture access ------------------------------------------------

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// Raw codestreams pass through; JP2 files yield the payload of their
    /// contiguous codestream box (I.5.4) via the I.4 box walk: LBox u32 +
    /// TBox 4 bytes; LBox = 1 means an XLBox u64 follows; LBox = 0 means
    /// the box runs to the end of the file.
    fn codestream_of(bytes: &[u8]) -> Vec<u8> {
        // SOC = 0xFF4F = bytes 255, 79 (Table A.4).
        if bytes.starts_with(&[255, 79]) {
            return bytes.to_vec();
        }
        let mut pos = 0usize;
        while pos + 8 <= bytes.len() {
            let lbox = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap());
            let tbox = &bytes[pos + 4..pos + 8];
            let (header, end) = match lbox {
                0 => (8, bytes.len()),
                1 => {
                    let xlbox = u64::from_be_bytes(bytes[pos + 8..pos + 16].try_into().unwrap());
                    (16, pos + usize::try_from(xlbox).unwrap())
                }
                n => (8, pos + n as usize),
            };
            if tbox == b"jp2c" {
                return bytes[pos + header..end].to_vec();
            }
            pos = end;
        }
        panic!("fixture has no contiguous codestream box");
    }

    // ---- the fixture zoo, transcribed from tests/fixtures/manifest.json -

    enum ZooQuant {
        /// "No quantization" (Table A.28 style 0): the zoo's reversible
        /// streams signal LL = depth then (depth+1, depth+1, depth+2) per
        /// level. Measured from the fixture bytes: the SPqcd bytes of
        /// gray-53-raw.j2k (offset 62) are 64, 72, 72, 80, ... and Table
        /// A.29 stores the exponent in the five MSBs: 64 >> 3 = 8,
        /// 72 >> 3 = 9, 80 >> 3 = 10.
        Reversible { depth: u8 },
        /// Scalar expounded (style 2): all four 9-7 fixtures carry the same
        /// sixteen Table A.30 words; see irreversible_97_steps.
        Irreversible97,
    }

    fn reversible_exponents(depth: u8, levels: u8) -> Vec<u8> {
        let mut v = vec![depth];
        for _ in 0..levels {
            v.extend([depth + 1, depth + 1, depth + 2]);
        }
        v
    }

    /// The (exponent, mantissa) pairs shared by the 9-7 fixtures, from the
    /// raw SPqcd words of rgb-97-raw.j2k (offset 68 on): the first word is
    /// 30496 = 14 * 2048 + 1824 so (14, 1824) per Table A.30 (exponent in
    /// the top five bits, mantissa in the low eleven); the last word is
    /// 22370 = 10 * 2048 + 1890 so (10, 1890).
    fn irreversible_97_steps() -> Vec<(u8, u16)> {
        vec![
            (14, 1824),
            (14, 1776),
            (14, 1776),
            (14, 1728),
            (13, 1792),
            (13, 1792),
            (13, 1760),
            (12, 1872),
            (12, 1872),
            (12, 1896),
            (10, 5),
            (10, 5),
            (10, 71),
            (10, 2003),
            (10, 2003),
            (10, 1890),
        ]
    }

    struct Zoo {
        file: &'static str,
        width: u32,
        height: u32,
        tile: (u32, u32),
        components: usize,
        depth: u8,
        progression: ProgressionOrder,
        layers: u16,
        levels: u8,
        cb_exp: (u8, u8),
        wavelet: WaveletKind,
        precincts: &'static [(u8, u8)],
        quant: ZooQuant,
        tile_parts: usize,
    }

    /// One untiled fixture with the zoo's shared defaults: single tile
    /// covering the image, LRCP, one layer, five decomposition levels,
    /// 64 x 64 code-blocks (signalled 4, so xcb = ycb = 6), maximal
    /// precincts, 8-bit unsigned samples.
    fn zoo_base(
        file: &'static str,
        width: u32,
        height: u32,
        components: usize,
        wavelet: WaveletKind,
        quant: ZooQuant,
    ) -> Zoo {
        Zoo {
            file,
            width,
            height,
            tile: (width, height),
            components,
            depth: 8,
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            levels: 5,
            cb_exp: (6, 6),
            wavelet,
            precincts: &[],
            quant,
            tile_parts: 1,
        }
    }

    /// Expectations for every manifest.json entry. Derivations from the
    /// manifest params: mode L / RGB / RGBA / I;16 gives the component
    /// count and depth; "irreversible" selects the Table A.20 wavelet;
    /// num_resolutions 3 means NL = 2; codeblock_size (16, 16) means
    /// signalled 4, so exponents (4, 4); quality_layers [50, 10, 2] means
    /// 3 layers; tile_size (128, 128) tiles a 523 x 311 image into
    /// ceil(523/128) * ceil(311/128) = 5 * 3 = 15 tile-parts (B-5);
    /// precinct_size (128, 128) is signalled per Table A.21 as the bytes
    /// 34, 51, 68, 85, 102, 119 (PPx low four bits, PPy high four bits:
    /// 34 = 2 + 2*16, ... 119 = 7 + 7*16 with 2^7 = 128 at r = NL).
    fn zoo() -> Vec<Zoo> {
        use super::ProgressionOrder as Po;
        use super::WaveletKind::{Irreversible97, Reversible53};
        let rev = || ZooQuant::Reversible { depth: 8 };
        vec![
            zoo_base("gray-53-jp2.jp2", 97, 61, 1, Reversible53, rev()),
            zoo_base(
                "gray-97-jp2.jp2",
                97,
                61,
                1,
                Irreversible97,
                ZooQuant::Irreversible97,
            ),
            zoo_base("rgb-53-jp2.jp2", 130, 83, 3, Reversible53, rev()),
            zoo_base(
                "rgb-97-jp2.jp2",
                130,
                83,
                3,
                Irreversible97,
                ZooQuant::Irreversible97,
            ),
            zoo_base("gray-53-raw.j2k", 97, 61, 1, Reversible53, rev()),
            zoo_base(
                "rgb-97-raw.j2k",
                130,
                83,
                3,
                Irreversible97,
                ZooQuant::Irreversible97,
            ),
            zoo_base("rgba-53-jp2.jp2", 64, 64, 4, Reversible53, rev()),
            Zoo {
                tile: (128, 128),
                tile_parts: 15,
                ..zoo_base("rgb-tiled.jp2", 523, 311, 3, Reversible53, rev())
            },
            Zoo {
                layers: 3,
                ..zoo_base(
                    "rgb-layers.jp2",
                    523,
                    311,
                    3,
                    Irreversible97,
                    ZooQuant::Irreversible97,
                )
            },
            Zoo {
                levels: 2,
                ..zoo_base("rgb-res3.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                cb_exp: (4, 4),
                ..zoo_base("rgb-cb16.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                precincts: &[(2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 7)],
                ..zoo_base("rgb-precinct.jp2", 523, 311, 3, Reversible53, rev())
            },
            zoo_base("rgb-prog-lrcp.jp2", 130, 83, 3, Reversible53, rev()),
            Zoo {
                progression: Po::Rlcp,
                ..zoo_base("rgb-prog-rlcp.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                progression: Po::Rpcl,
                ..zoo_base("rgb-prog-rpcl.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                progression: Po::Pcrl,
                ..zoo_base("rgb-prog-pcrl.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                progression: Po::Cprl,
                ..zoo_base("rgb-prog-cprl.jp2", 130, 83, 3, Reversible53, rev())
            },
            Zoo {
                depth: 16,
                ..zoo_base(
                    "gray16-53-jp2.jp2",
                    80,
                    50,
                    1,
                    Reversible53,
                    ZooQuant::Reversible { depth: 16 },
                )
            },
        ]
    }

    #[test]
    fn zoo_main_headers_match_the_manifest() {
        for z in zoo() {
            let raw = fixture(z.file);
            let bytes = codestream_of(&raw);
            let cs = parse(&bytes).unwrap_or_else(|e| panic!("{}: {e}", z.file));
            assert!(cs.warnings.is_empty(), "{}: {:?}", z.file, cs.warnings);

            let siz = &cs.main.siz;
            assert_eq!(siz.rsiz, 0, "{}", z.file);
            assert_eq!((siz.xsiz, siz.ysiz), (z.width, z.height), "{}", z.file);
            assert_eq!((siz.xosiz, siz.yosiz), (0, 0), "{}", z.file);
            assert_eq!((siz.xtsiz, siz.ytsiz), z.tile, "{}", z.file);
            assert_eq!((siz.xtosiz, siz.ytosiz), (0, 0), "{}", z.file);
            assert_eq!(siz.components.len(), z.components, "{}", z.file);
            for c in &siz.components {
                assert_eq!(c.depth, z.depth, "{}", z.file);
                assert!(!c.signed, "{}", z.file);
                assert_eq!((c.xrsiz, c.yrsiz), (1, 1), "{}", z.file);
            }

            let cod = &cs.main.cod;
            assert!(!cod.sop_markers && !cod.eph_markers, "{}", z.file);
            assert_eq!(cod.progression, z.progression, "{}", z.file);
            assert_eq!(cod.layers, z.layers, "{}", z.file);
            assert_eq!(cod.mct, 0, "{}", z.file);
            assert_eq!(cod.style.decomposition_levels, z.levels, "{}", z.file);
            assert_eq!(
                (
                    cod.style.code_block_width_exp,
                    cod.style.code_block_height_exp
                ),
                z.cb_exp,
                "{}",
                z.file
            );
            assert_eq!(cod.style.code_block_style, 0, "{}", z.file);
            assert_eq!(cod.style.wavelet, z.wavelet, "{}", z.file);
            let precincts: Vec<(u8, u8)> =
                cod.style.precincts.iter().map(|p| (p.ppx, p.ppy)).collect();
            assert_eq!(precincts, z.precincts, "{}", z.file);

            assert_eq!(cs.main.qcd.guard_bits, 2, "{}", z.file);
            match (&z.quant, &cs.main.qcd.style) {
                (ZooQuant::Reversible { depth }, QuantizationStyle::None { exponents }) => {
                    assert_eq!(
                        exponents,
                        &reversible_exponents(*depth, z.levels),
                        "{}",
                        z.file
                    );
                }
                (ZooQuant::Irreversible97, QuantizationStyle::ScalarExpounded { steps }) => {
                    let pairs: Vec<(u8, u16)> =
                        steps.iter().map(|s| (s.exponent, s.mantissa)).collect();
                    assert_eq!(pairs, irreversible_97_steps(), "{}", z.file);
                }
                (_, other) => panic!("{}: unexpected quantization {other:?}", z.file),
            }

            assert!(cs.main.coc.is_empty(), "{}", z.file);
            assert!(cs.main.qcc.is_empty(), "{}", z.file);
            assert!(cs.main.rgn.is_empty(), "{}", z.file);
            assert!(cs.main.poc.is_empty(), "{}", z.file);
            assert!(cs.main.ppm.is_empty(), "{}", z.file);

            assert_eq!(cs.tile_parts.len(), z.tile_parts, "{}", z.file);
            for (i, part) in cs.tile_parts.iter().enumerate() {
                assert_eq!(part.sot.tile_index as usize, i, "{}", z.file);
                assert_eq!(part.sot.tile_part_index, 0, "{}", z.file);
                assert_eq!(part.sot.tile_part_count, 1, "{}", z.file);
                // Psot spans SOT marker (2) + Lsot (10) + SOD (2) + body
                // (Figure A.16), so the body is Psot - 14 bytes.
                assert_eq!(
                    part.body.len() as u32,
                    part.sot.tile_part_length - 14,
                    "{}",
                    z.file
                );
            }
        }
    }

    // ---- delimiters and the tile-part walk ------------------------------

    #[test]
    fn psot_zero_final_tile_part_runs_to_eoc() {
        let mut s = tiny_main_header();
        // First part: Psot = 12 (SOT segment) + 2 (SOD) + 3 (body) = 17.
        s.extend(tile_part(0, 17, 0, 2, &[1, 2, 3]));
        // Last part: Psot = 0 extends to EOC (A.4.2).
        s.extend(tile_part(0, 0, 1, 2, &[4, 5, 6, 7]));
        s.extend(EOC.to_be_bytes());

        let cs = parse(&s).unwrap();
        assert!(cs.warnings.is_empty(), "{:?}", cs.warnings);
        assert_eq!(cs.tile_parts.len(), 2);
        assert_eq!(cs.tile_parts[0].body, [1, 2, 3]);
        assert_eq!(cs.tile_parts[0].sot.tile_part_index, 0);
        assert_eq!(cs.tile_parts[1].body, [4, 5, 6, 7]);
        assert_eq!(cs.tile_parts[1].sot.tile_part_index, 1);
        assert_eq!(cs.tile_parts[1].sot.tile_part_length, 0);
    }

    #[test]
    fn tnsot_undercount_is_tolerated_for_compatibility() {
        // The measured real-world shape: TNsot declares five tile-parts but
        // six ship (TPsot 0..=5) — a violation of A.4.2, which allows only
        // the correct count or zero. Extra parts parse anyway, never a
        // rejection, and keep their appearance order; the decode stage owns
        // the one-per-codestream compatibility summary.
        let mut s = tiny_main_header();
        for tpsot in 0..6u8 {
            s.extend(tile_part(0, 15, tpsot, 5, &[tpsot]));
        }
        s.extend(EOC.to_be_bytes());

        let cs = parse(&s).unwrap();
        assert_eq!(cs.tile_parts.len(), 6);
        for (i, part) in cs.tile_parts.iter().enumerate() {
            assert_eq!(part.sot.tile_part_index as usize, i);
            assert_eq!(part.sot.tile_part_count, 5);
            assert_eq!(part.body, [i as u8]);
        }
        assert!(cs.warnings.is_empty(), "{:?}", cs.warnings);
    }

    #[test]
    fn truncated_main_headers_error_never_panic() {
        let mut s = tiny_main_header();
        let sot_at = s.len();
        s.extend(tile_part(0, 17, 0, 1, &[1, 2, 3]));
        s.extend(EOC.to_be_bytes());
        parse(&s).unwrap();

        // Every truncation point before the first SOT is a main-header
        // problem: a hard error (leniency doctrine), never a panic.
        for cut in 0..sot_at {
            assert!(parse(&s[..cut]).is_err(), "prefix of {cut} bytes");
        }
    }

    #[test]
    fn malformed_main_headers_error_never_panic() {
        let tail = {
            let mut v = tile_part(0, 15, 0, 1, &[9]);
            v.extend(EOC.to_be_bytes());
            v
        };
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();

        // SIZ must immediately follow SOC (A.5.1).
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(COD, &tiny_cod_payload()));
        cases.push(("missing SIZ", v));

        // Duplicate SIZ: "only one SIZ per codestream" (A.5.1).
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(&tail);
        cases.push(("duplicate SIZ", v));

        // Duplicate COD: "one and only one in the main header" (A.6.1).
        let mut v = tiny_main_header();
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(&tail);
        cases.push(("duplicate COD", v));

        // Missing QCD: required in the main header (Figure A.3).
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(&tail);
        cases.push(("missing QCD", v));

        // A hostile segment length pointing past the end of the data.
        let mut v = tiny_main_header();
        v.extend(COM.to_be_bytes());
        v.extend(500u16.to_be_bytes());
        v.extend([1, 104, 105]);
        cases.push(("segment length overrun", v));

        // A "marker" without the mandatory 255 high byte (A.1.2).
        let mut v = tiny_main_header();
        v.extend([0, 5]);
        cases.push(("not a marker", v));

        // Csiz = 0 violates Table A.9 (1 to 16384).
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(0)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(&tail);
        cases.push(("zero components", v));

        // Zero layers violates Table A.14 (1 to 65535).
        let mut cod = tiny_cod_payload();
        cod[2] = 0;
        cod[3] = 0;
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &cod));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(&tail);
        cases.push(("zero layers", v));

        // Code-block exponent 9 violates Table A.18 (0 to 8).
        let mut cod = tiny_cod_payload();
        cod[6] = 9;
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &cod));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(&tail);
        cases.push(("code-block exponent 9", v));

        // Reserved quantization style 31 violates Table A.28.
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(1)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &[31, 40, 100]));
        v.extend(&tail);
        cases.push(("reserved quantization style", v));

        // Ssiz depth field 40 violates Table A.11 (0 to 37).
        let mut siz = tiny_siz_payload(1);
        let ssiz_at = siz.len() - 3;
        siz[ssiz_at] = 40;
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &siz));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(&tail);
        cases.push(("depth beyond 38 bits", v));

        // A POC whose body is not a whole number of 7-byte entries (A-6).
        let mut v = tiny_main_header();
        v.extend(seg(POC, &[0, 0, 0, 1, 1]));
        v.extend(&tail);
        cases.push(("ragged POC", v));

        // EOC with no tile-part: "at least one tile-part" (A.4).
        let mut v = tiny_main_header();
        v.extend(EOC.to_be_bytes());
        cases.push(("no tile-part", v));

        for (name, bytes) in cases {
            assert!(parse(&bytes).is_err(), "{name}");
        }
    }

    #[test]
    fn component_count_limit_trips_before_allocation() {
        let mut v = SOC.to_be_bytes().to_vec();
        v.extend(seg(SIZ, &tiny_siz_payload(17)));
        v.extend(seg(COD, &tiny_cod_payload()));
        v.extend(seg(QCD, &derived_qcd_payload()));
        v.extend(tile_part(0, 15, 0, 1, &[9]));
        v.extend(EOC.to_be_bytes());
        // Default max_components is 16; Csiz = 17 must trip the bound.
        match parse(&v) {
            Err(JpxError::LimitExceeded { what, actual, .. }) => {
                assert_eq!(what, "max_components");
                assert_eq!(actual, 17);
            }
            other => panic!("expected the component bound to trip, got {other:?}"),
        }
    }

    // ---- functional marker segments -------------------------------------

    #[test]
    fn functional_segments_parse_in_the_main_header() {
        let mut s = tiny_main_header();
        // COC (Figure A.10): Ccoc 0 (8-bit: Csiz < 257), Scoc 1 (precincts
        // signalled, Table A.23), SPcoc: NL 1, code-block 8 x 8 (signalled
        // 1), style 0, 9-7 wavelet, precinct bytes 18 = 2 + 1*16 (PPx 2,
        // PPy 1) and 51 = 3 + 3*16 (Table A.21).
        s.extend(seg(COC, &[0, 1, 1, 1, 1, 0, 0, 18, 51]));
        // QCC (Figure A.14): Cqcc 0, Sqcc 64 = 2 * 32 (guard 2, style 0),
        // Table A.29 exponents 64 >> 3 = 8, 72 >> 3 = 9, 80 >> 3 = 10.
        s.extend(seg(QCC, &[0, 64, 64, 72, 72, 80]));
        // RGN (Figure A.12): Crgn 0, Srgn 0 (maxshift), SPrgn 37.
        s.extend(seg(RGN, &[0, 0, 37]));
        // A second RGN with reserved style 3 (Table A.25): warn and skip.
        s.extend(seg(RGN, &[0, 3, 99]));
        // POC (Figure A.15), two 7-byte entries (Csiz < 257):
        //   (RSpoc 0, CSpoc 0, LYEpoc 1, REpoc 1, CEpoc 1, Ppoc 0 = LRCP)
        //   (RSpoc 1, CSpoc 0, LYEpoc 1, REpoc 6, CEpoc 0, Ppoc 4 = CPRL)
        s.extend(seg(POC, &[0, 0, 0, 1, 1, 1, 0, 1, 0, 0, 1, 6, 0, 4]));
        // PPM (Figure A.20): Zppm 0, Nppm = 1, one packed-header byte 7.
        s.extend(seg(PPM, &[0, 0, 0, 0, 1, 7]));
        // Parse-and-skip segments: TLM (A.7.1), CRG (A.9.1), COM (A.9.2).
        s.extend(seg(TLM, &[0, 0]));
        s.extend(seg(CRG, &[0, 0, 0, 0]));
        s.extend(seg(COM, &[0, 1, 104, 105]));
        s.extend(tile_part(0, 15, 0, 1, &[9]));
        s.extend(EOC.to_be_bytes());

        let cs = parse(&s).unwrap();

        assert_eq!(cs.main.coc.len(), 1);
        let coc = &cs.main.coc[0];
        assert_eq!(coc.component, 0);
        assert_eq!(coc.style.decomposition_levels, 1);
        assert_eq!(coc.style.code_block_width_exp, 3);
        assert_eq!(coc.style.code_block_height_exp, 3);
        assert_eq!(coc.style.wavelet, WaveletKind::Irreversible97);
        let precincts: Vec<(u8, u8)> = coc.style.precincts.iter().map(|p| (p.ppx, p.ppy)).collect();
        assert_eq!(precincts, [(2, 1), (3, 3)]);

        assert_eq!(cs.main.qcc.len(), 1);
        let qcc = &cs.main.qcc[0];
        assert_eq!(qcc.component, 0);
        assert_eq!(qcc.quant.guard_bits, 2);
        match &qcc.quant.style {
            QuantizationStyle::None { exponents } => {
                assert_eq!(exponents, &[8, 9, 9, 10]);
            }
            other => panic!("unexpected QCC style {other:?}"),
        }

        assert_eq!(cs.main.rgn.len(), 1);
        assert_eq!(cs.main.rgn[0].component, 0);
        assert_eq!(cs.main.rgn[0].shift, 37);

        assert_eq!(cs.main.poc.len(), 2);
        assert_eq!(cs.main.poc[0].order, ProgressionOrder::Lrcp);
        assert_eq!(cs.main.poc[0].layer_end, 1);
        assert_eq!(cs.main.poc[0].res_end, 1);
        assert_eq!(cs.main.poc[0].comp_end, 1);
        assert_eq!(cs.main.poc[1].order, ProgressionOrder::Cprl);
        assert_eq!(cs.main.poc[1].res_start, 1);
        assert_eq!(cs.main.poc[1].res_end, 6);
        // The signalled CEpoc 0 decodes as the component count (Table A.32).
        assert_eq!(cs.main.poc[1].comp_end, 1);

        assert_eq!(cs.main.ppm.len(), 1);
        assert_eq!(cs.main.ppm[0].index, 0);
        assert_eq!(cs.main.ppm[0].data, [0, 0, 0, 1, 7]);

        // Only the reserved-style RGN warned; the skip segments are silent.
        assert_eq!(cs.warnings.len(), 1, "{:?}", cs.warnings);
        assert!(cs.warnings[0].message.contains("RGN"), "{:?}", cs.warnings);
    }

    #[test]
    fn tile_part_header_overrides_are_recorded() {
        // A COD override with NL 2, plus QCC / PPT / PLT / COM, all in the
        // TPsot = 0 header where A.6 allows them.
        let mut cod2 = tiny_cod_payload();
        cod2[5] = 2;
        let mut header = seg(COD, &cod2);
        header.extend(seg(QCC, &[0, 64, 64, 72, 72, 80, 72, 72, 80]));
        header.extend(seg(PPT, &[0, 5, 6]));
        header.extend(seg(PLT, &[0, 3]));
        header.extend(seg(COM, &[0, 1, 104, 105]));
        let body = [1, 2];
        let psot = u32::try_from(12 + header.len() + 2 + body.len()).unwrap();

        let mut payload = 0u16.to_be_bytes().to_vec();
        payload.extend(psot.to_be_bytes());
        payload.extend([0, 1]);
        let mut s = tiny_main_header();
        s.extend(seg(SOT, &payload));
        s.extend(header);
        s.extend(SOD.to_be_bytes());
        s.extend(body);
        s.extend(EOC.to_be_bytes());

        let cs = parse(&s).unwrap();
        assert!(cs.warnings.is_empty(), "{:?}", cs.warnings);
        assert_eq!(cs.tile_parts.len(), 1);
        let o = &cs.tile_parts[0].overrides;
        assert_eq!(o.cod.as_ref().unwrap().style.decomposition_levels, 2);
        assert_eq!(o.qcc.len(), 1);
        assert_eq!(o.ppt.len(), 1);
        assert_eq!(o.ppt[0].index, 0);
        assert_eq!(o.ppt[0].data, [5, 6]);
        assert_eq!(cs.tile_parts[0].body, [1, 2]);
    }

    #[test]
    fn late_tile_part_coding_markers_warn_but_parse() {
        // COD is only legal in the TPsot = 0 header (A.6.1); in a later
        // part it is honoured leniently with a warning.
        let header = seg(COD, &tiny_cod_payload());
        let psot = u32::try_from(12 + header.len() + 2).unwrap();
        let mut payload = 0u16.to_be_bytes().to_vec();
        payload.extend(psot.to_be_bytes());
        payload.extend([1, 0]);

        let mut s = tiny_main_header();
        s.extend(tile_part(0, 15, 0, 0, &[9]));
        s.extend(seg(SOT, &payload));
        s.extend(header);
        s.extend(SOD.to_be_bytes());
        s.extend(EOC.to_be_bytes());

        let cs = parse(&s).unwrap();
        assert_eq!(cs.tile_parts.len(), 2);
        assert!(cs.tile_parts[1].overrides.cod.is_some());
        assert!(
            cs.warnings
                .iter()
                .any(|warning| warning.message.contains("COD")),
            "{:?}",
            cs.warnings
        );
    }

    #[test]
    fn corrupt_tile_territory_degrades_to_warnings() {
        // Psot pointing past the end of the data: truncated-tail doctrine.
        let mut s = tiny_main_header();
        s.extend(tile_part(0, 1000, 0, 1, &[1, 2, 3]));
        let cs = parse(&s).unwrap();
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].body, [1, 2, 3]);
        assert!(!cs.warnings.is_empty());

        // Garbage where the next SOT or EOC should be.
        let mut s = tiny_main_header();
        s.extend(tile_part(0, 15, 0, 1, &[7]));
        s.extend([0, 0, 0, 0]);
        let cs = parse(&s).unwrap();
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].body, [7]);
        assert!(!cs.warnings.is_empty());

        // Psot = 0 with the EOC missing entirely: keep the tail, warn.
        let mut s = tiny_main_header();
        s.extend(tile_part(0, 0, 0, 1, &[9, 9]));
        let cs = parse(&s).unwrap();
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].body, [9, 9]);
        assert!(
            cs.warnings
                .iter()
                .any(|warning| warning.message.contains("EOC")),
            "{:?}",
            cs.warnings
        );
    }

    // ---- packed packet headers ------------------------------------------

    #[test]
    fn split_packed_headers_concatenates_in_zppm_order() {
        // The Nppm/Ippm series (A.7.4): Nppm 3 + bytes 97 98 99 for the
        // first tile-part, Nppm 2 + bytes 100 101 for the second. Split so
        // the second length prefix straddles the segment boundary, and
        // hand the segments over in reversed appearance order so only the
        // Zppm index can restore the series.
        let series = [0, 0, 0, 3, 97, 98, 99, 0, 0, 0, 2, 100, 101];
        let segments = vec![
            Ppm {
                index: 1,
                data: series[9..].to_vec(),
            },
            Ppm {
                index: 0,
                data: series[..9].to_vec(),
            },
        ];
        let blobs = split_packed_headers(&segments, 2).unwrap();
        assert_eq!(blobs, [vec![97, 98, 99], vec![100, 101]]);

        // Entries beyond the requested tile-part count are ignored (a
        // truncated codestream may carry fewer tile-parts than the PPM
        // series describes).
        let blobs = split_packed_headers(&segments, 1).unwrap();
        assert_eq!(blobs, [vec![97, 98, 99]]);
    }

    #[test]
    fn split_packed_headers_rejects_truncated_series() {
        let segments = vec![Ppm {
            index: 0,
            data: vec![0, 0, 0, 3, 97, 98],
        }];
        // The declared 3-byte entry has only 2 bytes.
        assert!(split_packed_headers(&segments, 1).is_err());
        // A length prefix cut off halfway.
        let segments = vec![Ppm {
            index: 0,
            data: vec![0, 0],
        }];
        assert!(split_packed_headers(&segments, 1).is_err());
    }

    // ---- override merging and precedence --------------------------------

    fn style_with_levels(levels: u8) -> CodingStyle {
        CodingStyle {
            decomposition_levels: levels,
            code_block_width_exp: 6,
            code_block_height_exp: 6,
            code_block_style: 0,
            wavelet: WaveletKind::Reversible53,
            precincts: Vec::new(),
        }
    }

    fn cod_with(layers: u16, levels: u8) -> Cod {
        Cod {
            sop_markers: false,
            eph_markers: false,
            progression: ProgressionOrder::Lrcp,
            layers,
            mct: 0,
            style: style_with_levels(levels),
        }
    }

    fn quant_with_guard(guard: u8) -> Quantization {
        Quantization {
            guard_bits: guard,
            style: QuantizationStyle::ScalarDerived {
                exponent: 5,
                mantissa: 100,
            },
        }
    }

    fn poc_with_order(order: ProgressionOrder) -> PocSegment {
        PocSegment {
            res_start: 0,
            comp_start: 0,
            layer_end: 1,
            res_end: 1,
            comp_end: 1,
            order,
        }
    }

    fn sot_with_part(tile_part_index: u8) -> Sot {
        Sot {
            tile_index: 0,
            tile_part_length: 0,
            tile_part_index,
            tile_part_count: 0,
        }
    }

    fn tiny_siz_struct() -> Siz {
        Siz {
            rsiz: 0,
            xsiz: 16,
            ysiz: 16,
            xosiz: 0,
            yosiz: 0,
            xtsiz: 16,
            ytsiz: 16,
            xtosiz: 0,
            ytosiz: 0,
            components: vec![
                SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 1,
                    yrsiz: 1,
                },
                SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 1,
                    yrsiz: 1,
                },
            ],
        }
    }

    #[test]
    fn merge_tile_overrides_first_part_wins_and_orders_ppt() {
        let part0 = TilePart {
            sot: sot_with_part(0),
            overrides: TileOverrides {
                qcd: Some(quant_with_guard(1)),
                coc: vec![Coc {
                    component: 0,
                    style: style_with_levels(1),
                }],
                // Zppt out of order within the header: 1 before 0.
                ppt: vec![
                    Ppt {
                        index: 1,
                        data: vec![2],
                    },
                    Ppt {
                        index: 0,
                        data: vec![1],
                    },
                ],
                ..TileOverrides::default()
            },
            body: &[],
        };
        let part1 = TilePart {
            sot: sot_with_part(1),
            overrides: TileOverrides {
                cod: Some(cod_with(3, 2)),
                qcd: Some(quant_with_guard(2)),
                coc: vec![
                    Coc {
                        component: 0,
                        style: style_with_levels(2),
                    },
                    Coc {
                        component: 1,
                        style: style_with_levels(3),
                    },
                ],
                poc: vec![poc_with_order(ProgressionOrder::Rlcp)],
                ppt: vec![Ppt {
                    index: 0,
                    data: vec![3],
                }],
                ..TileOverrides::default()
            },
            body: &[],
        };

        let merged = merge_tile_overrides(&[&part0, &part1]).unwrap();
        // QCD: the TPsot = 0 header wins (A.6.4 usage).
        assert_eq!(merged.qcd.as_ref().unwrap().guard_bits, 1);
        // COD: absent from part 0, honoured leniently from part 1.
        assert_eq!(merged.cod.as_ref().unwrap().layers, 3);
        // COC: first occurrence per component wins.
        assert_eq!(merged.coc.len(), 2);
        assert_eq!(merged.coc[0].component, 0);
        assert_eq!(merged.coc[0].style.decomposition_levels, 1);
        assert_eq!(merged.coc[1].component, 1);
        assert_eq!(merged.coc[1].style.decomposition_levels, 3);
        // POC entries concatenate in tile-part order.
        assert_eq!(merged.poc.len(), 1);
        // PPT: Zppt order within each header, then tile-part order (A.7.5).
        let ppt_data: Vec<u8> = merged.ppt.iter().flat_map(|p| p.data.clone()).collect();
        assert_eq!(ppt_data, [1, 2, 3]);
    }

    #[test]
    fn resolve_tile_coding_precedence() {
        let main = MainHeader {
            siz: tiny_siz_struct(),
            cod: cod_with(10, 5),
            coc: Vec::new(),
            qcd: quant_with_guard(0),
            qcc: Vec::new(),
            rgn: Vec::new(),
            poc: vec![poc_with_order(ProgressionOrder::Rlcp)],
            ppm: Vec::new(),
        };

        // Tile COD and tile POC override the main header (A.6.1/A.6.6).
        let tile = TileOverrides {
            cod: Some(Cod {
                progression: ProgressionOrder::Rpcl,
                sop_markers: true,
                eph_markers: true,
                ..cod_with(20, 6)
            }),
            poc: vec![poc_with_order(ProgressionOrder::Cprl)],
            ..TileOverrides::default()
        };
        let coding = resolve_tile_coding(&main, &tile).unwrap();
        assert_eq!(coding.progression, ProgressionOrder::Rpcl);
        assert_eq!(coding.layers, 20);
        assert!(coding.sop_markers);
        assert!(coding.eph_markers);
        assert_eq!(coding.poc.len(), 1);
        assert_eq!(coding.poc[0].order, ProgressionOrder::Cprl);

        // With no tile overrides everything falls back to the main header.
        let coding = resolve_tile_coding(&main, &TileOverrides::default()).unwrap();
        assert_eq!(coding.progression, ProgressionOrder::Lrcp);
        assert_eq!(coding.layers, 10);
        assert!(!coding.sop_markers);
        assert_eq!(coding.poc[0].order, ProgressionOrder::Rlcp);
    }

    #[test]
    fn resolve_component_coding_precedence_ladder() {
        // Distinct decomposition levels / guard bits mark each rung of the
        // A.6.1 / A.6.4 ladder:
        //   tile COC (7) > tile COD (6) > main COC (4) > main COD (5)
        //   tile QCC (4) > tile QCD (1) > main QCC (3) > main QCD (0)
        let main = MainHeader {
            siz: tiny_siz_struct(),
            cod: cod_with(1, 5),
            coc: vec![Coc {
                component: 1,
                style: style_with_levels(4),
            }],
            qcd: quant_with_guard(0),
            qcc: vec![Qcc {
                component: 1,
                quant: quant_with_guard(3),
            }],
            rgn: vec![Rgn {
                component: 0,
                shift: 5,
            }],
            poc: Vec::new(),
            ppm: Vec::new(),
        };
        let tile = TileOverrides {
            cod: Some(cod_with(1, 6)),
            coc: vec![Coc {
                component: 0,
                style: style_with_levels(7),
            }],
            qcd: Some(quant_with_guard(1)),
            qcc: vec![Qcc {
                component: 0,
                quant: quant_with_guard(4),
            }],
            rgn: vec![Rgn {
                component: 0,
                shift: 9,
            }],
            ..TileOverrides::default()
        };

        // Component 0: tile COC, tile QCC, tile RGN all win.
        let c0 = resolve_component_coding(&main, &tile, 0).unwrap();
        assert_eq!(c0.style.decomposition_levels, 7);
        assert_eq!(c0.quant.guard_bits, 4);
        assert_eq!(c0.roi_shift, Some(9));

        // Component 1: no tile COC/QCC, so tile COD and tile QCD apply; no
        // RGN anywhere for it.
        let c1 = resolve_component_coding(&main, &tile, 1).unwrap();
        assert_eq!(c1.style.decomposition_levels, 6);
        assert_eq!(c1.quant.guard_bits, 1);
        assert_eq!(c1.roi_shift, None);

        // No tile overrides at all: main COC/QCC for component 1, main
        // COD/QCD for component 0, and the main RGN applies.
        let empty = TileOverrides::default();
        let c0 = resolve_component_coding(&main, &empty, 0).unwrap();
        assert_eq!(c0.style.decomposition_levels, 5);
        assert_eq!(c0.quant.guard_bits, 0);
        assert_eq!(c0.roi_shift, Some(5));
        let c1 = resolve_component_coding(&main, &empty, 1).unwrap();
        assert_eq!(c1.style.decomposition_levels, 4);
        assert_eq!(c1.quant.guard_bits, 3);
    }

    // ---- the shared (E-5) band-quantization resolution -------------------

    fn quant(style: QuantizationStyle) -> Quantization {
        Quantization {
            guard_bits: 2,
            style,
        }
    }

    #[test]
    fn band_quant_takes_listed_entries_and_derives_short_tails() {
        // NL = 2 has seven sub-bands: LL(nb 2), r1 HL/LH/HH (nb 2),
        // r2 HL/LH/HH (nb 1). A three-entry reversible list serves flat
        // indices 0..3 directly; the tail derives from the first entry
        // via (E-5): eps_b = eps_0 - NL + nb = 8 - 2 + nb.
        let reversible = quant(QuantizationStyle::None {
            exponents: vec![8, 9, 9],
        });
        for (flat, level, exponent) in [(0, 2, 8), (1, 2, 9), (2, 2, 9), (3, 2, 8), (4, 1, 7)] {
            let resolved = reversible.band_quant(2, level, flat);
            assert_eq!(
                resolved,
                BandQuant {
                    exponent,
                    mantissa: 0
                },
                "flat {flat}"
            );
        }
        assert!(reversible.short_for(2));
        assert!(!quant(QuantizationStyle::None {
            exponents: vec![8; 7],
        })
        .short_for(2));

        // Scalar derived always follows (E-5) with mu_0 and never counts
        // as short; the derivation clamps at zero for hostile headers
        // (eps_0 = 1, NL = 5, nb = 1 would go negative).
        let derived = quant(QuantizationStyle::ScalarDerived {
            exponent: 8,
            mantissa: 100,
        });
        assert_eq!(
            derived.band_quant(2, 1, 4),
            BandQuant {
                exponent: 7,
                mantissa: 100
            }
        );
        assert!(!derived.short_for(2));
        let hostile = quant(QuantizationStyle::ScalarDerived {
            exponent: 1,
            mantissa: 0,
        });
        assert_eq!(hostile.band_quant(5, 1, 13).exponent, 0);

        // An expounded short list derives (eps, mu) from its first entry.
        let expounded = quant(QuantizationStyle::ScalarExpounded {
            steps: vec![QuantStep {
                exponent: 8,
                mantissa: 50,
            }],
        });
        assert_eq!(
            expounded.band_quant(1, 1, 3),
            BandQuant {
                exponent: 8,
                mantissa: 50
            }
        );
        assert!(expounded.short_for(1));
    }
}
