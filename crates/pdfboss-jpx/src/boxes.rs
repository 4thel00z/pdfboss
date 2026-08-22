//! JP2 file-format box scan (ITU-T T.800 Annex I): walks the box structure
//! (I.4), collects the JP2 Header metadata (I.5.3) and hands back the
//! contiguous codestream (I.5.4) for the marker layer.
//!
//! Unknown boxes are skipped per I.8; a JPX-compatible `ftyp` brand and an
//! `rreq` box are treated as skippable, not as errors (real-world streams
//! carry them).

use crate::error::{JpxError, Result};
use crate::JpxWarning;

/// What the leading bytes of the input identify (the container sniff).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ContainerKind {
    /// JP2-family file: starts with the JPEG 2000 Signature box (I.5.1).
    Jp2,
    /// Raw codestream: starts with SOC (A.4.1) followed by SIZ (A.5.1).
    RawCodestream,
}

/// The 12-byte JPEG 2000 Signature box (I.5.1): LBox = 12, TBox = 'jP\x20\x20',
/// DBox = <CR><LF><0x87><LF>. Decimal to satisfy the no-hex-blob rule:
/// 0x0000000C 0x6A502020 0x0D0A870A.
fn jp2_signature() -> [u8; 12] {
    [0, 0, 0, 12, 106, 80, 32, 32, 13, 10, 135, 10]
}

/// SOC (0xFF4F, A.4.1) immediately followed by SIZ (0xFF51, A.5.1) — the
/// mandatory first four bytes of every raw codestream.
fn soc_siz_prefix() -> [u8; 4] {
    [255, 79, 255, 81]
}

/// Sniffs the container kind from the input prefix. This is the only
/// classification the decoder performs before committing to a parse:
/// anything else is `NotJpeg2000`.
pub(crate) fn sniff(data: &[u8]) -> Result<ContainerKind> {
    if data.len() >= 12 && data[..12] == jp2_signature() {
        return Ok(ContainerKind::Jp2);
    }
    if data.len() >= 4 && data[..4] == soc_siz_prefix() {
        return Ok(ContainerKind::RawCodestream);
    }
    Err(JpxError::NotJpeg2000)
}

/// Colour Specification box payload (colr, I.5.3.3).
// Constructed by the boxes stage; the variants are the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ColorSpec {
    /// METH = 1: enumerated colourspace. EnumCS 16 = sRGB, 17 = greyscale,
    /// 18 = sYCC (I.5.3.3); other values map to `ColorKind::Other`.
    Enumerated(u32),
    /// METH = 2: restricted ICC profile. The profile itself is not
    /// interpreted; the colour stage approximates by component count and
    /// records the guess (`ColorKind::IccGuess`).
    Icc {
        /// Byte length of the embedded profile (recorded for warnings).
        profile_len: u32,
    },
}

/// Palette box (pclr, I.5.3.4).
// Constructed by the boxes stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Palette {
    /// NE: number of palette entries (1..=1024).
    pub entries: u16,
    /// NPC: number of created channels.
    pub created_channels: u8,
    /// B_i per created channel: bits 0-6 store `depth - 1`, bit 7 the sign
    /// flag — kept raw exactly as read.
    pub channel_depths: Vec<u8>,
    /// C values, sign-extended to i32, laid out as
    /// `values[entry * created_channels + channel]`.
    pub values: Vec<i32>,
}

/// One Component Mapping box entry (cmap, I.5.3.5).
// Constructed by the boxes stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ComponentMapping {
    /// CMP: codestream component index.
    pub component: u16,
    /// MTYP: 0 = direct use, 1 = palette mapping via `palette_column`.
    pub mapping_type: u8,
    /// PCOL: palette column applied when `mapping_type == 1`.
    pub palette_column: u8,
}

/// One Channel Definition box entry (cdef, I.5.3.6).
// Constructed by the boxes stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChannelDefinition {
    /// Cn: channel index (post component-mapping).
    pub channel: u16,
    /// Typ: 0 = colour, 1 = opacity, 2 = premultiplied opacity,
    /// 65535 = unspecified.
    pub kind: u16,
    /// Asoc: 0 = whole image, k = colour k (1-based), 65535 = none.
    pub association: u16,
}

/// JP2 Header box contents (jp2h, I.5.3) that survive to the colour stage.
// Constructed by the boxes stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct Jp2Header {
    /// ihdr HEIGHT (I.5.3.1). Cross-checked against SIZ, never trusted over
    /// it.
    pub height: u32,
    /// ihdr WIDTH.
    pub width: u32,
    /// ihdr NC: component count.
    pub num_components: u16,
    /// ihdr BPC, raw: bits 0-6 store `depth - 1`, bit 7 the sign flag;
    /// 255 = depths vary and live in `component_depths` (bpcc, I.5.3.2).
    pub bit_depth: u8,
    /// bpcc payload (one raw B_i byte per component), empty unless
    /// `bit_depth == 255`.
    pub component_depths: Vec<u8>,
    /// colr (I.5.3.3). When several colr boxes appear the first METH the
    /// decoder understands wins, per the I.5.3.3 precedence note.
    pub color: ColorSpec,
    /// pclr palette (I.5.3.4), applied inside the crate before samples are
    /// handed out.
    pub palette: Option<Palette>,
    /// cmap entries (I.5.3.5); EMPTY means the identity mapping (direct
    /// use of each codestream component), which is mandatory when no
    /// palette box is present.
    pub component_mapping: Vec<ComponentMapping>,
    /// cdef entries (I.5.3.6); the colour stage reports an opacity channel
    /// with association 0 as `DecodedImage::alpha_index`.
    pub channel_definitions: Vec<ChannelDefinition>,
}

/// Result of the box scan.
// Constructed by the boxes stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct Container<'a> {
    /// JP2 header metadata; `None` for raw codestream inputs.
    pub header: Option<Jp2Header>,
    /// The contiguous codestream: the jp2c payload (I.5.4) for JP2 files,
    /// or the whole input for raw codestreams.
    pub codestream: &'a [u8],
    /// Soft findings (skipped boxes, rreq presence, jpx compatibility
    /// brand, trailing garbage), classified per [`JpxWarning::data_loss`].
    pub warnings: Vec<JpxWarning>,
}

/// Sniffs and, for JP2 files, walks the box structure (I.4: LBox/TBox with
/// LBox = 1 selecting a 64-bit XLBox and LBox = 0 meaning "to end of
/// file"), returning the codestream slice plus header metadata.
///
/// Hard errors: `NotJpeg2000` from the sniff, `Malformed` for a broken box
/// structure or a missing jp2c/jp2h. Unknown boxes are skipped with a
/// warning (I.8).
pub(crate) fn scan(data: &[u8]) -> Result<Container<'_>> {
    match sniff(data)? {
        ContainerKind::RawCodestream => Ok(Container {
            header: None,
            codestream: data,
            warnings: Vec::new(),
        }),
        ContainerKind::Jp2 => scan_jp2(data),
    }
}

// Box type codes from Table I.2 ('res\040' contains the \040 space), plus
// the reader-requirements box type carried by JPX-family files, which this
// reader only skips.
const TYPE_FTYP: [u8; 4] = *b"ftyp";
const TYPE_JP2H: [u8; 4] = *b"jp2h";
const TYPE_IHDR: [u8; 4] = *b"ihdr";
const TYPE_BPCC: [u8; 4] = *b"bpcc";
const TYPE_COLR: [u8; 4] = *b"colr";
const TYPE_PCLR: [u8; 4] = *b"pclr";
const TYPE_CMAP: [u8; 4] = *b"cmap";
const TYPE_CDEF: [u8; 4] = *b"cdef";
const TYPE_RES: [u8; 4] = *b"res ";
const TYPE_JP2C: [u8; 4] = *b"jp2c";
const TYPE_RREQ: [u8; 4] = *b"rreq";
// ftyp brands (Table I.3 defines 'jp2\040'; the JPX brands come from the
// extended file format that shares this box structure).
const BRAND_JP2: [u8; 4] = *b"jp2 ";
const BRAND_JPX: [u8; 4] = *b"jpx ";
const BRAND_JPXB: [u8; 4] = *b"jpxb";

fn malformed(detail: impl Into<String>) -> JpxError {
    JpxError::Malformed(detail.into())
}

/// Renders a box type for messages: the ISO/IEC 646 string when printable,
/// the raw bytes in decimal otherwise (I.4: types are named as character
/// strings).
fn type_name(kind: [u8; 4]) -> String {
    if kind.iter().all(|byte| (32..=126).contains(byte)) {
        kind.iter().map(|&byte| byte as char).collect()
    } else {
        format!("{kind:?}")
    }
}

fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// One decoded box header plus its payload slice.
struct RawBox<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
    /// Offset (within the buffer `read_box` was given) just past the box.
    end: usize,
}

/// Reads the box at `offset` per I.4: LBox is a 4-byte BE length counting
/// every field of the box; LBox = 1 selects the 8-byte BE XLBox (which
/// also counts itself); LBox = 0 means the box runs to the end of the
/// enclosing buffer; LBox 2..=7 is reserved for ISO use.
fn read_box(data: &[u8], offset: usize) -> Result<RawBox<'_>> {
    let rest = &data[offset..];
    if rest.len() < 8 {
        return Err(malformed(format!(
            "box header truncated: {} bytes left, 8 needed",
            rest.len()
        )));
    }
    let lbox = be32(rest);
    let kind = [rest[4], rest[5], rest[6], rest[7]];
    let (header_len, total): (usize, u64) = match lbox {
        0 => (8, rest.len() as u64),
        1 => {
            if rest.len() < 16 {
                return Err(malformed(format!(
                    "box '{}' truncated inside its XLBox field",
                    type_name(kind)
                )));
            }
            let xlbox = u64::from_be_bytes([
                rest[8], rest[9], rest[10], rest[11], rest[12], rest[13], rest[14], rest[15],
            ]);
            if xlbox < 16 {
                return Err(malformed(format!(
                    "box '{}' XLBox = {xlbox} is shorter than its own header",
                    type_name(kind)
                )));
            }
            (16, xlbox)
        }
        2..=7 => {
            return Err(malformed(format!(
                "box '{}' uses reserved length code {lbox}",
                type_name(kind)
            )));
        }
        length => (8, u64::from(length)),
    };
    if total > rest.len() as u64 {
        return Err(malformed(format!(
            "box '{}' claims {total} bytes but only {} remain",
            type_name(kind),
            rest.len()
        )));
    }
    let total = total as usize;
    Ok(RawBox {
        kind,
        payload: &rest[header_len..total],
        end: offset + total,
    })
}

/// The payload-to-EOF slice when the box at `offset` is a jp2c whose
/// declared LBox/XLBox length overruns the remaining bytes — the
/// signature of a truncated download. `None` for every other failure:
/// non-jp2c boxes and unreadable headers keep hard-failing.
fn jp2c_payload_to_eof(data: &[u8], offset: usize) -> Option<&[u8]> {
    let rest = data.get(offset..)?;
    if rest.len() < 8 || rest[4..8] != TYPE_JP2C {
        return None;
    }
    let header_len = match be32(rest) {
        // LBox = 0 runs to EOF and cannot overrun; reserved codes and a
        // header truncated inside its own XLBox stay hard errors (I.4).
        0 | 2..=7 => return None,
        1 => {
            if rest.len() < 16 {
                return None;
            }
            let xlbox = u64::from_be_bytes([
                rest[8], rest[9], rest[10], rest[11], rest[12], rest[13], rest[14], rest[15],
            ]);
            if xlbox < 16 || xlbox <= rest.len() as u64 {
                return None;
            }
            16
        }
        length => {
            if u64::from(length) <= rest.len() as u64 {
                return None;
            }
            8
        }
    };
    rest.get(header_len..)
}

/// Top-level walk of a JP2 file: signature (already verified by the
/// sniff), then ftyp (I.5.2, immediately after the signature), then any
/// mix of jp2h, jp2c and skippable boxes (I.8).
fn scan_jp2(data: &[u8]) -> Result<Container<'_>> {
    let mut warnings = Vec::new();
    // I.5.2: the File Type box shall immediately follow the signature box.
    let ftyp = read_box(data, 12)?;
    if ftyp.kind != TYPE_FTYP {
        return Err(malformed(format!(
            "expected the ftyp box after the signature, found '{}'",
            type_name(ftyp.kind)
        )));
    }
    check_ftyp(ftyp.payload, &mut warnings)?;

    let mut header: Option<Jp2Header> = None;
    let mut codestream: Option<&[u8]> = None;
    let mut offset = ftyp.end;
    while offset < data.len() {
        let next = match read_box(data, offset) {
            Ok(next) => next,
            Err(err) => {
                // A final jp2c whose declared length overruns EOF is the
                // shape of a truncated file: degrade to "runs to EOF" so
                // the truncation lands in the codestream, where the
                // marker layer already handles it leniently — exactly
                // like the identical truncated RAW codestream.
                if codestream.is_none() {
                    if let Some(payload) = jp2c_payload_to_eof(data, offset) {
                        // Truncation: the missing tail is missing pixels.
                        warnings.push(JpxWarning::loss(
                            "jp2c box length overruns the file; codestream truncated to EOF",
                        ));
                        codestream = Some(payload);
                        break;
                    }
                }
                // Everything needed to decode is in hand: a broken
                // trailing box is a soft finding, not a hard error.
                if header.is_some() && codestream.is_some() {
                    warnings.push(JpxWarning::note(format!(
                        "trailing garbage after the codestream: {err}"
                    )));
                    break;
                }
                return Err(err);
            }
        };
        match next.kind {
            TYPE_JP2H if header.is_none() => {
                header = Some(parse_jp2h(next.payload, &mut warnings)?);
            }
            TYPE_JP2H => {
                // I.5.3: one and only one JP2 Header box.
                warnings.push(JpxWarning::note("duplicate jp2h box skipped"));
            }
            TYPE_JP2C if codestream.is_none() => {
                if header.is_none() {
                    // I.5.4 forbids a codestream before the JP2 Header
                    // box; keep reading so the header can still be found.
                    warnings.push(JpxWarning::note("jp2c box appears before the jp2h box"));
                }
                codestream = Some(next.payload);
            }
            TYPE_JP2C => {
                // I.5.4: readers shall ignore codestreams after the first.
                warnings.push(JpxWarning::note("extra jp2c box ignored"));
            }
            TYPE_RREQ => {
                warnings.push(JpxWarning::note("reader-requirements (rreq) box skipped"));
            }
            other => {
                // I.8: skip and ignore boxes not defined by the spec.
                warnings.push(JpxWarning::note(format!(
                    "unknown box '{}' skipped",
                    type_name(other)
                )));
            }
        }
        offset = next.end;
    }
    let header = header.ok_or_else(|| malformed("missing jp2h box"))?;
    let codestream = codestream.ok_or_else(|| malformed("missing jp2c box"))?;
    Ok(Container {
        header: Some(header),
        codestream,
        warnings,
    })
}

/// Validates the File Type box payload (I.5.2): brand 'jp2\040' is fully
/// readable; the JPX brands (or a JP2/JPX compatibility entry) are
/// accepted read-only with a warning; anything else is not a file this
/// reader may interpret.
fn check_ftyp(payload: &[u8], warnings: &mut Vec<JpxWarning>) -> Result<()> {
    if payload.len() < 8 {
        return Err(malformed(
            "ftyp box too short for its brand and minor version",
        ));
    }
    let brand = [payload[0], payload[1], payload[2], payload[3]];
    // MinV shall be zero, but readers shall continue regardless (I.5.2).
    if be32(&payload[4..]) != 0 {
        warnings.push(JpxWarning::note("ftyp minor version is not zero"));
    }
    let (entries, remainder) = payload[8..].as_chunks::<4>();
    let mut compatible = false;
    let mut jpx_compatible = false;
    for entry in entries {
        let code = *entry;
        compatible = compatible || code == BRAND_JP2;
        jpx_compatible = jpx_compatible || code == BRAND_JPX || code == BRAND_JPXB;
    }
    if !remainder.is_empty() {
        warnings.push(JpxWarning::note(format!(
            "ftyp compatibility list has {} trailing bytes",
            remainder.len()
        )));
    }
    if brand == BRAND_JP2 {
        return Ok(());
    }
    if brand == BRAND_JPX || brand == BRAND_JPXB {
        warnings.push(JpxWarning::note(format!(
            "JPX brand '{}': reading the JP2-compatible subset",
            type_name(brand)
        )));
        return Ok(());
    }
    // I.5.2: with a foreign brand, a 'jp2\040' compatibility entry means a
    // JP2 reader can still interpret the file.
    if compatible || jpx_compatible {
        warnings.push(JpxWarning::note(format!(
            "unknown brand '{}' with a JP2-compatible entry: reading as JP2",
            type_name(brand)
        )));
        return Ok(());
    }
    Err(malformed(format!(
        "ftyp brand '{}' with no JP2-compatible entry",
        type_name(brand)
    )))
}

/// Walks the JP2 Header superbox (I.5.3): ihdr first, then bpcc, colr,
/// pclr, cmap, cdef in any order; res and unknown boxes are skipped.
fn parse_jp2h(payload: &[u8], warnings: &mut Vec<JpxWarning>) -> Result<Jp2Header> {
    // I.5.3.1: the contents shall start with the Image Header box.
    let ihdr = read_box(payload, 0)?;
    if ihdr.kind != TYPE_IHDR {
        return Err(malformed(format!(
            "jp2h must start with ihdr, found '{}'",
            type_name(ihdr.kind)
        )));
    }
    let (height, width, num_components, bit_depth) = parse_ihdr(ihdr.payload, warnings)?;

    let mut component_depths: Vec<u8> = Vec::new();
    let mut color: Option<ColorSpec> = None;
    let mut palette: Option<Palette> = None;
    let mut component_mapping: Vec<ComponentMapping> = Vec::new();
    let mut channel_definitions: Vec<ChannelDefinition> = Vec::new();
    let mut offset = ihdr.end;
    while offset < payload.len() {
        let next = read_box(payload, offset)?;
        match next.kind {
            TYPE_BPCC if component_depths.is_empty() => {
                component_depths = parse_bpcc(next.payload, num_components)?;
            }
            TYPE_COLR if color.is_none() => {
                // I.5.3.3: the first colr box a reader understands wins;
                // reserved METH values make the whole box ignorable.
                color = parse_colr(next.payload, warnings)?;
            }
            TYPE_COLR => {
                // Additional colr boxes offer alternative specifications
                // of the SAME colourspace; ignoring them is conforming.
            }
            TYPE_PCLR if palette.is_none() => {
                palette = Some(parse_pclr(next.payload)?);
            }
            TYPE_CMAP if component_mapping.is_empty() => {
                component_mapping = parse_cmap(next.payload, num_components, warnings)?;
            }
            TYPE_CDEF if channel_definitions.is_empty() => {
                channel_definitions = parse_cdef(next.payload)?;
            }
            TYPE_BPCC | TYPE_PCLR | TYPE_CMAP | TYPE_CDEF => {
                // I.5.3.2/I.5.3.4/I.5.3.5/I.5.3.6: at most one of each.
                warnings.push(JpxWarning::note(format!(
                    "duplicate {} box skipped",
                    type_name(next.kind)
                )));
            }
            TYPE_IHDR => {
                // I.5.3.1: instances elsewhere shall be ignored.
                warnings.push(JpxWarning::note("extra ihdr box skipped"));
            }
            TYPE_RES => {
                // I.5.3.7: grid resolution only; nothing this decoder
                // needs.
            }
            other => {
                warnings.push(JpxWarning::note(format!(
                    "unknown box '{}' inside jp2h skipped",
                    type_name(other)
                )));
            }
        }
        offset = next.end;
    }

    // I.5.3.1: BPC = 255 promises a Bits Per Component box.
    if bit_depth == 255 && component_depths.is_empty() {
        return Err(malformed("ihdr BPC = 255 but no bpcc box present"));
    }
    // I.5.3.2: bpcc shall not be found when the depth is constant.
    if bit_depth != 255 && !component_depths.is_empty() {
        warnings.push(JpxWarning::note(
            "bpcc box present although ihdr BPC is uniform",
        ));
        component_depths.clear();
    }
    let color = color.ok_or_else(|| malformed("jp2h has no usable colr box"))?;
    // I.5.3.4: pclr and cmap require each other.
    if let Some(palette) = &palette {
        if component_mapping.is_empty() {
            return Err(malformed("pclr box without a cmap box"));
        }
        for entry in &component_mapping {
            if entry.mapping_type == 1
                && u16::from(entry.palette_column) >= u16::from(palette.created_channels)
            {
                return Err(malformed(format!(
                    "cmap references palette column {} of {}",
                    entry.palette_column, palette.created_channels
                )));
            }
        }
    } else if !component_mapping.is_empty() {
        if component_mapping
            .iter()
            .any(|entry| entry.mapping_type == 1)
        {
            return Err(malformed("cmap palette mapping without a pclr box"));
        }
        warnings.push(JpxWarning::note("cmap box present without a pclr box"));
    }
    Ok(Jp2Header {
        height,
        width,
        num_components,
        bit_depth,
        component_depths,
        color,
        palette,
        component_mapping,
        channel_definitions,
    })
}

/// Image Header box payload (I.5.3.1, exactly 14 bytes): HEIGHT, WIDTH,
/// NC, BPC, C, UnkC, IPR.
fn parse_ihdr(payload: &[u8], warnings: &mut Vec<JpxWarning>) -> Result<(u32, u32, u16, u8)> {
    // I.5.3.1: the box length shall be 22 bytes -> 14 payload bytes.
    if payload.len() != 14 {
        return Err(malformed(format!(
            "ihdr payload is {} bytes, must be 14",
            payload.len()
        )));
    }
    let height = be32(payload);
    let width = be32(&payload[4..]);
    let num_components = be16(&payload[8..]);
    let bit_depth = payload[10];
    let compression = payload[11];
    // Table I.5 ranges: HEIGHT/WIDTH 1.., NC 1..=16384, C = 7.
    if height == 0 || width == 0 {
        return Err(malformed("ihdr image size is zero"));
    }
    if num_components == 0 || num_components > 16384 {
        return Err(malformed(format!(
            "ihdr NC = {num_components} outside 1..=16384"
        )));
    }
    if compression != 7 {
        return Err(malformed(format!(
            "ihdr compression type {compression}, only 7 is defined"
        )));
    }
    check_depth_byte(bit_depth, true, "ihdr BPC")?;
    // UnkC and IPR (Table I.5): only 0 and 1 are defined; the decoder
    // needs neither, so odd values are soft findings.
    if payload[12] > 1 {
        warnings.push(JpxWarning::note(format!(
            "ihdr UnkC = {} is reserved",
            payload[12]
        )));
    }
    if payload[13] > 1 {
        warnings.push(JpxWarning::note(format!(
            "ihdr IPR = {} is reserved",
            payload[13]
        )));
    }
    Ok((height, width, num_components, bit_depth))
}

/// Validates a raw B/BPC depth byte (Tables I.6/I.8/I.13): the low 7 bits
/// hold `depth - 1` and shall stay within 0..=37; the high bit is the
/// sign flag. 255 means "depths vary" and is only legal where noted.
fn check_depth_byte(raw: u8, allow_varies: bool, what: &str) -> Result<()> {
    if raw == 255 && allow_varies {
        return Ok(());
    }
    if raw & 127 > 37 {
        return Err(malformed(format!(
            "{what} depth {} exceeds the 38-bit maximum",
            (raw & 127) as u32 + 1
        )));
    }
    Ok(())
}

/// Bits Per Component box payload (I.5.3.2): one raw depth byte per
/// codestream component, exactly NC of them.
fn parse_bpcc(payload: &[u8], num_components: u16) -> Result<Vec<u8>> {
    if payload.len() != usize::from(num_components) {
        return Err(malformed(format!(
            "bpcc holds {} depth bytes for {num_components} components",
            payload.len()
        )));
    }
    for &raw in payload {
        check_depth_byte(raw, false, "bpcc component")?;
    }
    Ok(payload.to_vec())
}

/// Colour Specification box payload (I.5.3.3): METH, PREC, APPROX, then
/// EnumCS (METH = 1) or an ICC profile (METH = 2). Reserved METH values
/// make the whole box ignorable (Table I.9), reported as `None`.
fn parse_colr(payload: &[u8], warnings: &mut Vec<JpxWarning>) -> Result<Option<ColorSpec>> {
    if payload.len() < 3 {
        return Err(malformed("colr box shorter than METH/PREC/APPROX"));
    }
    // PREC and APPROX shall be zero and shall be ignored by readers.
    match payload[0] {
        1 => {
            if payload.len() < 7 {
                return Err(malformed("colr METH 1 is missing its EnumCS field"));
            }
            if payload.len() > 7 {
                // I.5.3.3: EnumCS shall be the last field in the box.
                warnings.push(JpxWarning::note(
                    "colr METH 1 has trailing bytes after EnumCS",
                ));
            }
            Ok(Some(ColorSpec::Enumerated(be32(&payload[3..]))))
        }
        2 => Ok(Some(ColorSpec::Icc {
            profile_len: (payload.len() - 3) as u32,
        })),
        other => {
            // Table I.9: reserved METH -> ignore the entire box.
            warnings.push(JpxWarning::note(format!(
                "colr METH {other} is reserved; box ignored"
            )));
            Ok(None)
        }
    }
}

/// Palette box payload (I.5.3.4): NE, NPC, the per-column depth bytes,
/// then NE x NPC values in component-major order. Each value occupies
/// ceil(depth / 8) bytes with the actual value in the low-order bits;
/// signed columns sign-extend from their depth.
fn parse_pclr(payload: &[u8]) -> Result<Palette> {
    if payload.len() < 3 {
        return Err(malformed("pclr box shorter than NE and NPC"));
    }
    let entries = be16(payload);
    let created_channels = payload[2];
    // Table I.12: NE in 1..=1024, NPC in 1..=255.
    if entries == 0 || entries > 1024 {
        return Err(malformed(format!("pclr NE = {entries} outside 1..=1024")));
    }
    if created_channels == 0 {
        return Err(malformed("pclr NPC = 0"));
    }
    let depths_end = 3 + usize::from(created_channels);
    let channel_depths = payload
        .get(3..depths_end)
        .ok_or_else(|| malformed("pclr truncated inside its depth bytes"))?
        .to_vec();
    let mut widths = Vec::with_capacity(channel_depths.len());
    for &raw in &channel_depths {
        check_depth_byte(raw, false, "pclr column")?;
        let depth = u32::from(raw & 127) + 1;
        // The frozen seam stores palette samples as i32: depths past 32
        // bits (T.800 allows up to 38) cannot be represented.
        if depth > 32 {
            return Err(JpxError::Unsupported("palette column wider than 32 bits"));
        }
        widths.push((depth as usize).div_ceil(8));
    }
    let mut values = Vec::with_capacity(usize::from(entries) * usize::from(created_channels));
    let mut offset = depths_end;
    for _ in 0..entries {
        for (&raw, &width) in channel_depths.iter().zip(&widths) {
            let bytes = payload
                .get(offset..offset + width)
                .ok_or_else(|| malformed("pclr truncated inside its palette values"))?;
            offset += width;
            let mut value: u64 = 0;
            for &byte in bytes {
                value = value << 8 | u64::from(byte);
            }
            let depth = u32::from(raw & 127) + 1;
            // I.5.3.4: the value lives in the low `depth` bits.
            value &= (1u64 << depth) - 1;
            let signed = raw & 128 != 0;
            let extended: i64 = if signed && value >> (depth - 1) & 1 == 1 {
                value as i64 - (1i64 << depth)
            } else {
                value as i64
            };
            let value = i32::try_from(extended).map_err(|_| {
                JpxError::Unsupported("palette value outside the 32-bit signed range")
            })?;
            values.push(value);
        }
    }
    if offset != payload.len() {
        return Err(malformed(format!(
            "pclr has {} bytes after its last palette value",
            payload.len() - offset
        )));
    }
    Ok(Palette {
        entries,
        created_channels,
        channel_depths,
        values,
    })
}

/// Component Mapping box payload (I.5.3.5): 4 bytes per created channel —
/// CMP (2), MTYP (1), PCOL (1). The channel count is the box length / 4.
fn parse_cmap(
    payload: &[u8],
    num_components: u16,
    warnings: &mut Vec<JpxWarning>,
) -> Result<Vec<ComponentMapping>> {
    if payload.is_empty() || !payload.len().is_multiple_of(4) {
        return Err(malformed(format!(
            "cmap payload of {} bytes is not a run of 4-byte entries",
            payload.len()
        )));
    }
    let mut entries = Vec::with_capacity(payload.len() / 4);
    for chunk in payload.as_chunks::<4>().0 {
        let component = be16(chunk);
        let mapping_type = chunk[2];
        let palette_column = chunk[3];
        if component >= num_components {
            return Err(malformed(format!(
                "cmap references component {component} of {num_components}"
            )));
        }
        // Table I.14: values 2..=255 are reserved.
        if mapping_type > 1 {
            return Err(malformed(format!("cmap MTYP {mapping_type} is reserved")));
        }
        // I.5.3.5: PCOL shall be 0 for direct use.
        if mapping_type == 0 && palette_column != 0 {
            warnings.push(JpxWarning::note(format!(
                "cmap direct-use entry carries PCOL = {palette_column}"
            )));
        }
        entries.push(ComponentMapping {
            component,
            mapping_type,
            palette_column,
        });
    }
    Ok(entries)
}

/// Channel Definition box payload (I.5.3.6): N, then N descriptions of
/// Cn, Typ, Asoc (2 bytes each).
fn parse_cdef(payload: &[u8]) -> Result<Vec<ChannelDefinition>> {
    if payload.len() < 2 {
        return Err(malformed("cdef box shorter than its channel count"));
    }
    let count = be16(payload);
    // Table I.19: N ranges from 1; the payload is exactly 2 + 6N bytes.
    if count == 0 {
        return Err(malformed("cdef declares zero channel descriptions"));
    }
    if payload.len() != 2 + usize::from(count) * 6 {
        return Err(malformed(format!(
            "cdef declares {count} descriptions in a {}-byte payload",
            payload.len()
        )));
    }
    Ok(payload[2..]
        .as_chunks::<6>()
        .0
        .iter()
        .map(|chunk| ChannelDefinition {
            channel: be16(chunk),
            kind: be16(&chunk[2..]),
            association: be16(&chunk[4..]),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_recognizes_the_jp2_signature_box() {
        // I.5.1: the file must start with the 12-byte signature box
        // 0x0000000C 'jP\x20\x20' 0x0D0A870A; in decimal:
        // 0,0,0,12, 106,80,32,32, 13,10,135,10.
        let mut data = vec![0, 0, 0, 12, 106, 80, 32, 32, 13, 10, 135, 10];
        data.extend([0, 0, 0, 20]); // arbitrary following box bytes
        assert_eq!(sniff(&data).unwrap(), ContainerKind::Jp2);
    }

    #[test]
    fn sniff_recognizes_a_raw_codestream() {
        // A.4.1/A.5.1: SOC = 0xFF4F (255, 79) immediately followed by
        // SIZ = 0xFF51 (255, 81).
        let data = [255, 79, 255, 81, 0, 41];
        assert_eq!(sniff(&data).unwrap(), ContainerKind::RawCodestream);
    }

    #[test]
    fn sniff_rejects_everything_else() {
        // A PDF header, an empty input, a lone SOC without SIZ, and a
        // truncated signature box must all be NotJpeg2000.
        for bad in [
            b"%PDF-1.7 not an image".as_slice(),
            b"".as_slice(),
            &[255, 79, 255, 144],
            &[0, 0, 0, 12, 106, 80, 32, 32],
        ] {
            assert!(matches!(sniff(bad), Err(JpxError::NotJpeg2000)));
        }
    }

    #[test]
    fn scan_propagates_the_sniff_error() {
        assert!(matches!(scan(b"junk"), Err(JpxError::NotJpeg2000)));
    }

    // ---- test helpers -------------------------------------------------

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
    }

    /// Wraps `payload` in an I.4 box: LBox (4-byte BE, includes the 8
    /// header bytes) + TBox + DBox.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&u32::to_be_bytes(8 + payload.len() as u32));
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    /// Wraps `payload` using the LBox = 1 escape: XLBox is the 8-byte BE
    /// total length including LBox, TBox and XLBox themselves (I.4).
    fn xl_boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + payload.len());
        out.extend_from_slice(&u32::to_be_bytes(1));
        out.extend_from_slice(kind);
        out.extend_from_slice(&u64::to_be_bytes(16 + payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    /// ftyp payload (I.5.2): BR 'jp2\040', MinV 0, one CL entry 'jp2\040'.
    fn jp2_ftyp() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"jp2 ");
        payload.extend_from_slice(&u32::to_be_bytes(0));
        payload.extend_from_slice(b"jp2 ");
        boxed(b"ftyp", &payload)
    }

    /// ihdr payload (I.5.3.1, 14 bytes): HEIGHT, WIDTH, NC, BPC, C = 7
    /// (Table I.5), UnkC = 0, IPR = 0.
    fn ihdr_payload(height: u32, width: u32, nc: u16, bpc: u8) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&height.to_be_bytes());
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&nc.to_be_bytes());
        payload.push(bpc);
        payload.push(7); // C: compression type, shall be 7
        payload.push(0); // UnkC
        payload.push(0); // IPR
        payload
    }

    /// colr payload for METH = 1 (I.5.3.3): METH, PREC = 0, APPROX = 0,
    /// EnumCS as 4-byte BE.
    fn enumerated_colr(enum_cs: u32) -> Vec<u8> {
        let mut payload = vec![1, 0, 0];
        payload.extend_from_slice(&enum_cs.to_be_bytes());
        payload
    }

    /// Assembles signature + ftyp + jp2h(children) + jp2c(codestream).
    fn build_jp2(jp2h_children: &[Vec<u8>], codestream: &[u8]) -> Vec<u8> {
        let mut file = jp2_signature().to_vec();
        file.extend_from_slice(&jp2_ftyp());
        let jp2h: Vec<u8> = jp2h_children.iter().flatten().copied().collect();
        file.extend_from_slice(&boxed(b"jp2h", &jp2h));
        file.extend_from_slice(&boxed(b"jp2c", codestream));
        file
    }

    /// A structurally minimal 1-component greyscale jp2h child list.
    fn minimal_jp2h() -> Vec<Vec<u8>> {
        vec![
            boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
            boxed(b"colr", &enumerated_colr(17)),
        ]
    }

    fn soc_siz() -> Vec<u8> {
        soc_siz_prefix().to_vec()
    }

    // ---- fixtures vs manifest ------------------------------------------

    #[test]
    fn raw_codestream_passes_through_whole_input() {
        // manifest.json: gray-53-raw.j2k and rgb-97-raw.j2k are raw
        // codestreams (no JP2 container). scan must hand back the whole
        // input untouched with no header.
        for name in ["gray-53-raw.j2k", "rgb-97-raw.j2k"] {
            let data = fixture(name);
            let container = scan(&data).unwrap();
            assert!(container.header.is_none(), "{name}");
            assert_eq!(container.codestream.len(), data.len(), "{name}");
            assert!(container.warnings.is_empty(), "{name}");
        }
    }

    #[test]
    fn zoo_fixture_headers_match_the_manifest() {
        // Ground truth from tests/fixtures/manifest.json. The manifest
        // "size" is [width, height]; "mode" maps to ihdr/colr values as:
        //   L     -> NC = 1, BPC = 7   (8-bit unsigned, Table I.6:
        //            depth - 1 = 7, high bit 0), EnumCS 17 greyscale
        //            (Table I.10)
        //   I;16  -> NC = 1, BPC = 15  (16-bit unsigned), EnumCS 17
        //   RGB   -> NC = 3, BPC = 7, EnumCS 16 sRGB
        //   RGBA  -> NC = 4, BPC = 7, EnumCS 16 sRGB
        // Spot-checked by hand against the bytes of gray-53-jp2.jp2,
        // gray16-53-jp2.jp2, rgb-53-jp2.jp2, rgb-tiled.jp2 and
        // rgba-53-jp2.jp2 (ihdr payload at offset 48: HEIGHT, WIDTH, NC,
        // BPC; colr EnumCS at offset 73).
        let expected: &[(&str, u32, u32, u16, u8, u32)] = &[
            // (file, width, height, nc, bpc, enum_cs)
            ("gray-53-jp2.jp2", 97, 61, 1, 7, 17),
            ("gray-97-jp2.jp2", 97, 61, 1, 7, 17),
            ("gray16-53-jp2.jp2", 80, 50, 1, 15, 17),
            ("rgb-53-jp2.jp2", 130, 83, 3, 7, 16),
            ("rgb-97-jp2.jp2", 130, 83, 3, 7, 16),
            ("rgb-tiled.jp2", 523, 311, 3, 7, 16),
            ("rgb-layers.jp2", 523, 311, 3, 7, 16),
            ("rgb-res3.jp2", 130, 83, 3, 7, 16),
            ("rgb-cb16.jp2", 130, 83, 3, 7, 16),
            ("rgb-precinct.jp2", 523, 311, 3, 7, 16),
            ("rgb-prog-lrcp.jp2", 130, 83, 3, 7, 16),
            ("rgb-prog-rlcp.jp2", 130, 83, 3, 7, 16),
            ("rgb-prog-rpcl.jp2", 130, 83, 3, 7, 16),
            ("rgb-prog-pcrl.jp2", 130, 83, 3, 7, 16),
            ("rgb-prog-cprl.jp2", 130, 83, 3, 7, 16),
            ("rgba-53-jp2.jp2", 64, 64, 4, 7, 16),
        ];
        for &(name, width, height, nc, bpc, enum_cs) in expected {
            let data = fixture(name);
            let container = scan(&data).unwrap();
            let header = container.header.as_ref().unwrap_or_else(|| {
                panic!("{name}: JP2 file without header");
            });
            assert_eq!(header.width, width, "{name} width");
            assert_eq!(header.height, height, "{name} height");
            assert_eq!(header.num_components, nc, "{name} nc");
            assert_eq!(header.bit_depth, bpc, "{name} bpc");
            assert!(header.component_depths.is_empty(), "{name} bpcc");
            match header.color {
                ColorSpec::Enumerated(value) => {
                    assert_eq!(value, enum_cs, "{name} EnumCS");
                }
                ColorSpec::Icc { .. } => panic!("{name}: unexpected ICC colr"),
            }
            assert!(header.palette.is_none(), "{name} palette");
            assert!(header.component_mapping.is_empty(), "{name} cmap");
            // The jp2c payload is a codestream: SOC (0xFF4F) then SIZ
            // (0xFF51), A.4.1/A.5.1.
            assert!(
                container.codestream.len() >= 4 && container.codestream[..4] == soc_siz_prefix(),
                "{name}: codestream does not start with SOC + SIZ",
            );
        }
    }

    #[test]
    fn rgb_fixture_codestream_is_the_jp2c_payload() {
        // Hand-parsed layout of rgb-53-jp2.jp2 (414 bytes total):
        //   offset  0: signature box, 12 bytes (I.5.1)
        //   offset 12: ftyp, LBox = 20
        //   offset 32: jp2h, LBox = 45 (0x2D)
        //   offset 77: jp2c, LBox = 337 (0x151); 77 + 337 = 414 = EOF
        // so the codestream payload is 337 - 8 = 329 bytes starting at
        // offset 85.
        let data = fixture("rgb-53-jp2.jp2");
        assert_eq!(data.len(), 414);
        let container = scan(&data).unwrap();
        assert_eq!(container.codestream.len(), 329);
        assert_eq!(container.codestream, &data[85..414]);
        assert!(container.warnings.is_empty());
    }

    #[test]
    fn rgba_fixture_reports_the_cdef_channels() {
        // Hand-parsed cdef box of rgba-53-jp2.jp2 (offset 77, LBox = 34):
        // N = 4 with (Cn, Typ, Asoc) entries (0,0,1) (1,0,2) (2,0,3)
        // (3,1,0): channels 0..2 are the R, G, B colours (Typ 0, Asoc
        // 1..3, Table I.18) and channel 3 is whole-image opacity (Typ 1,
        // Asoc 0; Tables I.16/I.17).
        let data = fixture("rgba-53-jp2.jp2");
        let container = scan(&data).unwrap();
        let header = container.header.unwrap();
        let expected = [(0, 0, 1), (1, 0, 2), (2, 0, 3), (3, 1, 0)];
        assert_eq!(header.channel_definitions.len(), expected.len());
        for (definition, &(channel, kind, association)) in
            header.channel_definitions.iter().zip(&expected)
        {
            assert_eq!(definition.channel, channel);
            assert_eq!(definition.kind, kind);
            assert_eq!(definition.association, association);
        }
    }

    // ---- hand-built containers -----------------------------------------

    #[test]
    fn full_feature_container_parses_every_header_box() {
        let mut file = jp2_signature().to_vec();

        // ftyp with a JPX brand: readable because 'jp2 ' appears in the
        // compatibility list (I.5.2); recorded as a warning.
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"jpx ");
        ftyp.extend_from_slice(&u32::to_be_bytes(0));
        ftyp.extend_from_slice(b"jpxb");
        ftyp.extend_from_slice(b"jp2 ");
        file.extend_from_slice(&boxed(b"ftyp", &ftyp));

        // A reader-requirements box (defined outside this spec): skipped
        // per I.8.
        file.extend_from_slice(&boxed(b"rreq", &[5, 1, 2, 3, 4, 5]));

        let mut children: Vec<Vec<u8>> = Vec::new();
        // ihdr: 5 x 3, two components, BPC = 255 (depths vary, I.5.3.1).
        children.push(boxed(b"ihdr", &ihdr_payload(5, 3, 2, 255)));
        // bpcc (I.5.3.2): component 0 is 8-bit unsigned (7); component 1
        // is 10-bit signed (128 + 9 = 137).
        children.push(boxed(b"bpcc", &[7, 137]));
        // First colr is METH = 2 (restricted ICC, I.5.3.3) with a 20-byte
        // profile; it wins over the following METH = 1 box.
        let mut icc = vec![2, 0, 0];
        icc.extend_from_slice(&[9; 20]);
        children.push(boxed(b"colr", &icc));
        children.push(boxed(b"colr", &enumerated_colr(16)));
        // pclr (I.5.3.4): NE = 2 entries, NPC = 3 columns with depths
        //   B0 = 7   -> 8-bit unsigned, stored in 1 byte
        //   B1 = 9   -> 10-bit unsigned, stored in 2 bytes (low 10 bits)
        //   B2 = 135 -> 8-bit signed (128 + 7), stored in 1 byte
        // Entry values, component-major:
        //   entry 0: 200; [2, 3] = 2 * 256 + 3 = 515; 255 -> sign bit set
        //            in 8 bits -> 255 - 256 = -1
        //   entry 1: 1; [3, 255] = 3 * 256 + 255 = 1023; 127 -> +127
        let mut pclr = Vec::new();
        pclr.extend_from_slice(&u16::to_be_bytes(2)); // NE
        pclr.push(3); // NPC
        pclr.extend_from_slice(&[7, 9, 135]); // B_i
        pclr.extend_from_slice(&[200, 2, 3, 255]); // entry 0
        pclr.extend_from_slice(&[1, 3, 255, 127]); // entry 1
        children.push(boxed(b"pclr", &pclr));
        // cmap (I.5.3.5): three palette channels from component 0 plus a
        // direct channel from component 1.
        let mut cmap = Vec::new();
        for column in 0..3u8 {
            cmap.extend_from_slice(&u16::to_be_bytes(0)); // CMP
            cmap.push(1); // MTYP: palette mapping
            cmap.push(column); // PCOL
        }
        cmap.extend_from_slice(&u16::to_be_bytes(1));
        cmap.push(0); // MTYP: direct
        cmap.push(0); // PCOL shall be 0 for direct use
        children.push(boxed(b"cmap", &cmap));
        // cdef (I.5.3.6): channel 3 is whole-image opacity.
        let mut cdef = Vec::new();
        cdef.extend_from_slice(&u16::to_be_bytes(1)); // N
        cdef.extend_from_slice(&u16::to_be_bytes(3)); // Cn
        cdef.extend_from_slice(&u16::to_be_bytes(1)); // Typ: opacity
        cdef.extend_from_slice(&u16::to_be_bytes(0)); // Asoc: whole image
        children.push(boxed(b"cdef", &cdef));
        // res (I.5.3.7): known but unused; skipped without a warning.
        let resc = boxed(b"resc", &[0, 1, 0, 1, 0, 1, 0, 1, 0, 0]);
        children.push(boxed(b"res ", &resc));
        // An unknown box inside jp2h: skipped per I.8.
        children.push(boxed(b"blah", &[1, 2, 3]));
        let jp2h: Vec<u8> = children.iter().flatten().copied().collect();
        file.extend_from_slice(&boxed(b"jp2h", &jp2h));

        // jp2c via the LBox = 1 / XLBox 64-bit escape (I.4).
        let mut codestream = soc_siz();
        codestream.extend_from_slice(&[0, 41]);
        file.extend_from_slice(&xl_boxed(b"jp2c", &codestream));

        // A trailing LBox = 0 box: contains all bytes to EOF (I.4).
        let mut trailer = Vec::new();
        trailer.extend_from_slice(&u32::to_be_bytes(0));
        trailer.extend_from_slice(b"blah");
        trailer.extend_from_slice(&[7; 5]);
        file.extend_from_slice(&trailer);

        let container = scan(&file).unwrap();
        assert_eq!(container.codestream, codestream.as_slice());
        let header = container.header.unwrap();
        assert_eq!(header.height, 5);
        assert_eq!(header.width, 3);
        assert_eq!(header.num_components, 2);
        assert_eq!(header.bit_depth, 255);
        assert_eq!(header.component_depths, vec![7, 137]);
        match header.color {
            ColorSpec::Icc { profile_len } => assert_eq!(profile_len, 20),
            ColorSpec::Enumerated(value) => panic!("expected ICC, got EnumCS {value}"),
        }
        let palette = header.palette.unwrap();
        assert_eq!(palette.entries, 2);
        assert_eq!(palette.created_channels, 3);
        assert_eq!(palette.channel_depths, vec![7, 9, 135]);
        assert_eq!(palette.values, vec![200, 515, -1, 1, 1023, 127]);
        assert_eq!(header.component_mapping.len(), 4);
        for (column, entry) in header.component_mapping[..3].iter().enumerate() {
            assert_eq!(entry.component, 0);
            assert_eq!(entry.mapping_type, 1);
            assert_eq!(entry.palette_column, column as u8);
        }
        assert_eq!(header.component_mapping[3].component, 1);
        assert_eq!(header.component_mapping[3].mapping_type, 0);
        assert_eq!(header.channel_definitions.len(), 1);
        assert_eq!(header.channel_definitions[0].channel, 3);
        assert_eq!(header.channel_definitions[0].kind, 1);
        assert_eq!(header.channel_definitions[0].association, 0);
        // Soft findings: the JPX brand, the skipped rreq box, and the two
        // skipped unknown 'blah' boxes.
        assert!(container.warnings.len() >= 3, "{:?}", container.warnings);
    }

    #[test]
    fn palette_fields_are_masked_to_their_declared_depth() {
        // I.5.3.4: "the actual value shall be stored in the low-order
        // bits of the padded value" — a hostile file can set the padding
        // bits, which must be masked off. A 10-bit unsigned column value
        // stored as [255, 3]: 255 * 256 + 3 = 65283; masked to 10 bits:
        // 65283 mod 1024 = 771. A 10-bit signed column (B = 137... here
        // B = 128 + 9) with low bits 1023 = all ones -> -1 after sign
        // extension from bit 9.
        let mut pclr = Vec::new();
        pclr.extend_from_slice(&u16::to_be_bytes(1)); // NE
        pclr.push(2); // NPC
        pclr.extend_from_slice(&[9, 137]); // B: 10-bit unsigned, 10-bit signed
        pclr.extend_from_slice(&[255, 3]); // 65283 -> masked 771
        pclr.extend_from_slice(&[3, 255]); // 1023 -> sign-extended -1
        let mut cmap = Vec::new();
        for column in 0..2u8 {
            cmap.extend_from_slice(&u16::to_be_bytes(0));
            cmap.push(1);
            cmap.push(column);
        }
        let children = vec![
            boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
            boxed(b"colr", &enumerated_colr(17)),
            boxed(b"pclr", &pclr),
            boxed(b"cmap", &cmap),
        ];
        let file = build_jp2(&children, &soc_siz());
        let header = scan(&file).unwrap().header.unwrap();
        let palette = header.palette.unwrap();
        assert_eq!(palette.values, vec![771, -1]);
    }

    #[test]
    fn length_zero_jp2c_extends_to_end_of_file() {
        // I.4: LBox = 0 means "all bytes up to the end of the file".
        let mut file = jp2_signature().to_vec();
        file.extend_from_slice(&jp2_ftyp());
        let jp2h: Vec<u8> = minimal_jp2h().iter().flatten().copied().collect();
        file.extend_from_slice(&boxed(b"jp2h", &jp2h));
        let mut codestream = soc_siz();
        codestream.extend_from_slice(&[0, 41, 8, 9, 10]);
        file.extend_from_slice(&u32::to_be_bytes(0));
        file.extend_from_slice(b"jp2c");
        file.extend_from_slice(&codestream);
        let container = scan(&file).unwrap();
        assert_eq!(container.codestream, codestream.as_slice());
    }

    #[test]
    fn jp2c_before_jp2h_is_tolerated_with_a_warning() {
        // I.5.4 forbids a codestream box before the JP2 Header box, but a
        // reader that has both can still decode; be lenient and say so.
        let mut file = jp2_signature().to_vec();
        file.extend_from_slice(&jp2_ftyp());
        let codestream = soc_siz();
        file.extend_from_slice(&boxed(b"jp2c", &codestream));
        let jp2h: Vec<u8> = minimal_jp2h().iter().flatten().copied().collect();
        file.extend_from_slice(&boxed(b"jp2h", &jp2h));
        let container = scan(&file).unwrap();
        assert_eq!(container.codestream, codestream.as_slice());
        assert!(container.header.is_some());
        assert!(!container.warnings.is_empty());
    }

    // ---- hostile lengths -----------------------------------------------

    #[test]
    fn every_truncated_prefix_errors_cleanly_or_degrades_to_eof() {
        // rgb-53-jp2.jp2 ends with an explicit-length jp2c whose payload
        // starts at byte 85. Any prefix that ends before the payload has
        // no usable codestream and must fail cleanly; from the payload's
        // first byte on, the final jp2c's overrunning length degrades to
        // "truncated to EOF" with a warning — the marker layer owns the
        // codestream damage from there, exactly as it would for the
        // identical truncated raw codestream. None may panic.
        let data = fixture("rgb-53-jp2.jp2");
        assert_eq!(&data[81..85], b"jp2c");
        let payload_start = 85;
        for len in 0..data.len() {
            let result = scan(&data[..len]);
            if len < payload_start {
                assert!(result.is_err(), "prefix of {len} bytes unexpectedly parsed");
            } else {
                let container =
                    result.unwrap_or_else(|e| panic!("prefix of {len} bytes failed hard: {e}"));
                assert!(
                    container
                        .warnings
                        .iter()
                        .any(|warning| warning.message.contains("truncated to EOF")),
                    "prefix of {len} bytes lacks the truncation note: {:?}",
                    container.warnings
                );
            }
        }
        let container = scan(&data).unwrap();
        assert!(container.warnings.is_empty(), "{:?}", container.warnings);
    }

    #[test]
    fn oversized_and_reserved_box_lengths_error_cleanly() {
        // A jp2h declaring more bytes than the file holds.
        let mut oversized = jp2_signature().to_vec();
        oversized.extend_from_slice(&jp2_ftyp());
        oversized.extend_from_slice(&u32::to_be_bytes(1000));
        oversized.extend_from_slice(b"jp2h");
        oversized.extend_from_slice(&[0; 16]);
        assert!(matches!(scan(&oversized), Err(JpxError::Malformed(_))));

        // LBox values 2-7 are reserved for ISO use (I.4).
        let mut reserved = jp2_signature().to_vec();
        reserved.extend_from_slice(&jp2_ftyp());
        reserved.extend_from_slice(&u32::to_be_bytes(5));
        reserved.extend_from_slice(b"jp2h");
        reserved.extend_from_slice(&[0; 16]);
        assert!(matches!(scan(&reserved), Err(JpxError::Malformed(_))));

        // XLBox counts its own 16 header bytes (I.4): a value below 16 is
        // impossible, and a huge value must not overflow or panic.
        for xlbox in [10u64, u64::MAX] {
            let mut bad = jp2_signature().to_vec();
            bad.extend_from_slice(&jp2_ftyp());
            bad.extend_from_slice(&u32::to_be_bytes(1));
            bad.extend_from_slice(b"jp2h");
            bad.extend_from_slice(&xlbox.to_be_bytes());
            bad.extend_from_slice(&[0; 32]);
            assert!(
                matches!(scan(&bad), Err(JpxError::Malformed(_))),
                "XLBox {xlbox}"
            );
        }
    }

    #[test]
    fn structural_requirements_are_enforced() {
        // ftyp shall immediately follow the signature box (I.5.2).
        let mut no_ftyp = jp2_signature().to_vec();
        let jp2h: Vec<u8> = minimal_jp2h().iter().flatten().copied().collect();
        no_ftyp.extend_from_slice(&boxed(b"jp2h", &jp2h));
        no_ftyp.extend_from_slice(&boxed(b"jp2c", &soc_siz()));
        assert!(matches!(scan(&no_ftyp), Err(JpxError::Malformed(_))));

        // An unknown brand with no 'jp2 '/JPX compatibility entry cannot
        // be interpreted (I.5.2 / Table I.3).
        let mut alien = jp2_signature().to_vec();
        let mut ftyp = Vec::new();
        ftyp.extend_from_slice(b"abcd");
        ftyp.extend_from_slice(&u32::to_be_bytes(0));
        ftyp.extend_from_slice(b"efgh");
        alien.extend_from_slice(&boxed(b"ftyp", &ftyp));
        alien.extend_from_slice(&boxed(b"jp2h", &jp2h));
        alien.extend_from_slice(&boxed(b"jp2c", &soc_siz()));
        assert!(matches!(scan(&alien), Err(JpxError::Malformed(_))));

        // Missing jp2h, and missing jp2c.
        let mut no_header = jp2_signature().to_vec();
        no_header.extend_from_slice(&jp2_ftyp());
        no_header.extend_from_slice(&boxed(b"jp2c", &soc_siz()));
        assert!(matches!(scan(&no_header), Err(JpxError::Malformed(_))));
        let mut no_codestream = jp2_signature().to_vec();
        no_codestream.extend_from_slice(&jp2_ftyp());
        no_codestream.extend_from_slice(&boxed(b"jp2h", &jp2h));
        assert!(matches!(scan(&no_codestream), Err(JpxError::Malformed(_))));
    }

    #[test]
    fn jp2h_content_rules_are_enforced() {
        let checks: &[(&str, Vec<Vec<u8>>)] = &[
            // ihdr shall be the first box in jp2h (I.5.3.1).
            (
                "colr before ihdr",
                vec![
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                ],
            ),
            // The Image Header box length shall be 22 bytes -> 14-byte
            // payload (I.5.3.1).
            (
                "short ihdr",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)[..13]),
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // C (compression type) shall be 7 (Table I.5).
            (
                "bad compression type",
                vec![
                    {
                        let mut payload = ihdr_payload(5, 3, 1, 7);
                        payload[11] = 8;
                        boxed(b"ihdr", &payload)
                    },
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // HEIGHT and WIDTH range is 1.. (Table I.5).
            (
                "zero width",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 0, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // NC range is 1..=16384 (Table I.5).
            (
                "zero components",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 0, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // BPC = 255 requires a bpcc box (I.5.3.1/I.5.3.2).
            (
                "bpc 255 without bpcc",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 2, 255)),
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // bpcc must carry exactly NC depth bytes (I.5.3.2).
            (
                "bpcc count mismatch",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 2, 255)),
                    boxed(b"bpcc", &[7]),
                    boxed(b"colr", &enumerated_colr(17)),
                ],
            ),
            // At least one Colour Specification box is required (I.5.3).
            (
                "missing colr",
                vec![boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7))],
            ),
            // METH = 1 without the 4 EnumCS bytes (Table I.11).
            (
                "truncated colr",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &[1, 0, 0, 0]),
                ],
            ),
            // A Palette box requires a Component Mapping box (I.5.3.4).
            (
                "pclr without cmap",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"pclr", &[0, 1, 1, 7, 42]),
                ],
            ),
            // cmap PCOL must name an existing palette column (I.5.3.5).
            (
                "cmap palette column out of range",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"pclr", &[0, 1, 1, 7, 42]),
                    boxed(b"cmap", &[0, 0, 1, 3]),
                ],
            ),
            // cmap CMP must name an existing codestream component
            // (Table I.15 caps it, and NC bounds it).
            (
                "cmap component out of range",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"cmap", &[0, 9, 0, 0]),
                ],
            ),
            // NE range is 1..=1024 (Table I.12): 2000 entries is illegal.
            (
                "palette entry count out of range",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"pclr", &[7, 208, 1, 7, 42]),
                    boxed(b"cmap", &[0, 0, 1, 0]),
                ],
            ),
            // pclr truncated mid-values.
            (
                "palette values truncated",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"pclr", &[0, 2, 1, 7, 42]),
                    boxed(b"cmap", &[0, 0, 1, 0]),
                ],
            ),
            // cdef payload must be exactly 2 + 6N bytes (I.5.3.6).
            (
                "cdef length mismatch",
                vec![
                    boxed(b"ihdr", &ihdr_payload(5, 3, 1, 7)),
                    boxed(b"colr", &enumerated_colr(17)),
                    boxed(b"cdef", &[0, 2, 0, 0, 0, 1, 0, 0]),
                ],
            ),
        ];
        for (what, children) in checks {
            let file = build_jp2(children, &soc_siz());
            assert!(
                matches!(scan(&file), Err(JpxError::Malformed(_))),
                "{what}: expected Malformed",
            );
        }
    }
}
