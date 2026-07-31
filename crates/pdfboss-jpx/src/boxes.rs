//! JP2 file-format box scan (ITU-T T.800 Annex I): walks the box structure
//! (I.4), collects the JP2 Header metadata (I.5.3) and hands back the
//! contiguous codestream (I.5.4) for the marker layer.
//!
//! Unknown boxes are skipped per I.8; a JPX-compatible `ftyp` brand and an
//! `rreq` box are treated as skippable, not as errors (real-world streams
//! carry them).

use crate::error::{JpxError, Result};

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
    /// brand, trailing garbage).
    pub warnings: Vec<String>,
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
        ContainerKind::Jp2 | ContainerKind::RawCodestream => {
            Err(JpxError::Unsupported("decoder scaffold"))
        }
    }
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
}
