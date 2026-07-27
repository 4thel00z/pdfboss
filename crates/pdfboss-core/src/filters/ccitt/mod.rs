//! Group 3 and Group 4 facsimile coding (ITU-T T.4 and ITU-T T.6).
//!
//! One codec serves two callers. The PDF `CCITTFaxDecode` filter
//! (ISO 32000-1 §7.4.6) is the larger of them, which is why this module sits
//! beside the other stream filters rather than inside the JBIG2 one; a JBIG2
//! generic region whose MMR flag is set (ITU-T T.88 §6.2.6) is the other, and
//! reaches the same two-dimensional decoder.
//!
//! Both forms code a row of a bilevel image as runs of alternating colour,
//! each run written with a variable-length code. Everything here is therefore
//! built on a *bit* stream rather than a byte stream: codes straddle byte
//! boundaries as a matter of course, and the only byte alignment that exists
//! anywhere in the format is the one `/EncodedByteAlign` asks for. That bit
//! stream is the `bits` module.

use std::fmt;

use crate::error::{Error, Result};
use crate::filters::jbig2::bitmap::{Bitmap, MAX_PIXELS};
use crate::filters::{bool_parm, int_parm};
use crate::object::Dict;
use decoder::Params;

pub(crate) mod bits;
pub(crate) mod codes;
pub(crate) mod decoder;

#[cfg(test)]
pub(crate) mod testing;

/// A failure decoding a T.4 or T.6 stream.
///
/// The distinction that matters is between a stream this build *cannot* read
/// and a stream that is not a facsimile stream at all. Only the former is
/// [`CcittError::Unimplemented`]; a bit pattern outside the code tables is
/// corruption, and is reported rather than guessed at, because a guess
/// propagates into every row below it.
///
/// Running out of data is deliberately *not* an error here. A real scanner
/// truncates, and the row decoder keeps the rows it managed to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CcittError {
    /// A bit pattern matching no code in the table.
    UnknownCode,
    /// A run that would extend past the end of the row, or a chain of make-up
    /// codes describing a run wider than any row this build will decode.
    RunTooLong,
    /// A decoded construct that cannot be applied to the row it appears in —
    /// most importantly a coding mode that would leave the row's reference
    /// position where it was, which is how a corrupt stream asks to be decoded
    /// forever.
    Malformed(&'static str),
    /// A construct this build does not decode, such as the two-dimensional
    /// extension escape of ITU-T T.6 §2.2.
    Unimplemented(&'static str),
    /// A parameter the caller supplied that cannot describe an image.
    ///
    /// This is the caller's mistake rather than the stream's: it is settled
    /// before a bit is read.
    BadParameter(&'static str),
    /// An image whose declared size is past what this build will allocate.
    TooLarge {
        /// The refused width, in pixels.
        width: u32,
        /// The refused height, in pixels.
        height: u32,
    },
}

impl fmt::Display for CcittError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CcittError::UnknownCode => f.write_str("no such facsimile code"),
            CcittError::RunTooLong => f.write_str("run length past the end of the row"),
            CcittError::Malformed(what) => write!(f, "malformed facsimile stream: {what}"),
            CcittError::Unimplemented(what) => write!(f, "unsupported facsimile coding: {what}"),
            CcittError::BadParameter(what) => write!(f, "unusable facsimile parameter: {what}"),
            CcittError::TooLarge { width, height } => {
                write!(f, "facsimile image too large: {width} by {height}")
            }
        }
    }
}

impl From<CcittError> for Error {
    fn from(err: CcittError) -> Error {
        Error::Decode(err.to_string())
    }
}

/// Decodes a `CCITTFaxDecode` stream into packed 1-bit samples (ISO 32000-1
/// §7.4.6).
///
/// Every entry of Table 11 is accounted for. Five of the eight decide the
/// image:
///
/// - `/K` (default 0) selects the coding: below zero pure two-dimensional, 0
///   pure one-dimensional, above zero a mixture in which each row carries a bit
///   saying which it is.
/// - `/Columns` (default 1728) and `/Rows` (default 0, meaning "as many as the
///   data holds") are the image's dimensions. See [`MAX_IMAGE_PIXELS`] for what
///   bounds them.
/// - `/EncodedByteAlign` (default false) starts each row on a byte boundary.
/// - `/BlackIs1` (default false) is the polarity switch; see [`pack_samples`].
///
/// `/EndOfLine` (default false) decides less than it appears to. It is read and
/// passed on, but not to decide whether an end-of-line pattern is *recognised*:
/// producers set the flag inconsistently, and a decoder that trusts it over the
/// bits reads a row separator as twelve bits of image data, which ruins every
/// row after. The pattern is stepped over wherever it appears. What the flag
/// genuinely settles is whether *fill* bits may precede one, and that is what it
/// is passed on for.
///
/// The last two decide nothing here, and are not read:
///
/// - `/EndOfBlock` (default true) claims an end-of-facsimile block terminates
///   the data. That block is two consecutive end-of-line patterns, which are
///   not a row under any coding, so it ends the image whether or not the flag
///   claims it — the same reasoning as for `/EndOfLine`. The one case where the
///   flag could settle something is `/EndOfBlock` false together with `/Rows`
///   0, which states neither a height nor a terminator; counting the rows the
///   data holds is both the only answer available there and the one `/Rows` 0
///   already asks for.
/// - `/DamagedRowsBeforeError` (default 0) raises the number of damaged rows
///   tolerated before the decode fails. Table 11 gives it meaning only when
///   `/EndOfLine` is true and `/K` is non-negative, because tolerating a
///   damaged row means resynchronising on the next end-of-line pattern and
///   inventing the pixels in between. This build does not invent pixels: it
///   reports the damage, which is what the default asks for and stricter than
///   what any other value asks for.
pub(crate) fn decode_pdf_stream(data: &[u8], parms: Option<&Dict>) -> Result<Vec<u8>> {
    let params = Params {
        columns: columns_parm(parms)?,
        rows: rows_parm(parms)?,
        k: int_parm(parms, "K", 0).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        end_of_line: bool_parm(parms, "EndOfLine", false),
        byte_align: bool_parm(parms, "EncodedByteAlign", false),
    };
    // Both dimensions are settled above, so the bitmap the decoder allocates is
    // bounded before a bit of the coded data is read.
    if u64::from(params.columns) * u64::from(params.rows) > MAX_IMAGE_PIXELS {
        return Err(Error::Decode(format!(
            "CCITTFaxDecode image of {} by {} pixels exceeds the decoding limit",
            params.columns, params.rows,
        )));
    }
    let page = decoder::decode(data, &params)?;
    Ok(pack_samples(&page, bool_parm(parms, "BlackIs1", false)))
}

/// The largest facsimile image this filter will decode, in pixels.
///
/// Neither dimension is trustworthy. `/Columns` is stated but stated by the
/// file, and `/Rows` may be 0, which Table 11 defines as "however many rows the
/// data holds" — a count chosen by data that is equally the file's. So the
/// bound is applied the way the JBIG2 work budget in this crate applies its
/// own: from the *declared* dimensions, before the loop that would spend them,
/// so that a stream too expensive to decode is refused without a pixel being
/// decoded. The figure is the bitmap allocation cap, because a decode that
/// could not be stored is work that could only end in a refusal.
///
/// Where there is no declared height the same number bounds the count: the row
/// counter stops once the rows it has found would fill this many pixels. That
/// pass is bounded a second way as well, structurally — no row can be coded in
/// fewer bits than the shortest code, so a stream of `n` bytes holds fewer than
/// `8n` rows however it is written — but the pixel bound is the one that does
/// not depend on the code tables being what this build thinks they are.
const MAX_IMAGE_PIXELS: u64 = MAX_PIXELS;

/// Packs a decoded page into the 1-bit samples the image layer expects,
/// applying `/BlackIs1`.
///
/// **This is the one place a facsimile pixel becomes a sample bit**, and both
/// settings are routed through it deliberately, because the direction is easy
/// to get backwards and a page decoded upside-down in colour looks like a page.
///
/// Inside the codec a set pixel is black, as it is in JBIG2. `/DeviceGray` at
/// one bit per component reads a 0 sample as black. ISO 32000-1 Table 11
/// defaults `/BlackIs1` to *false*, meaning 0 bits are black in the decoded
/// output — which is already the `/DeviceGray` convention. So:
///
/// - `/BlackIs1` false, the default: invert, turning a set (black) pixel into a
///   0 sample.
/// - `/BlackIs1` true: do not invert, leaving a set pixel as a 1 sample.
///
/// Note that this is the *opposite* of the `JBIG2Decode` arm of
/// [`crate::filters::decode_stream`], which has no such parameter and therefore
/// always inverts. The two filters sit side by side in that match, and the
/// reason they differ is only this: JBIG2 defines 1 as ink, while
/// `CCITTFaxDecode` was given a parameter whose default already agrees with
/// `/DeviceGray`.
///
/// The padding bits at the end of a short row are white under both settings —
/// 1 when the default inverts them, 0 when `/BlackIs1` leaves them — so neither
/// setting draws a stripe down the right edge of an image whose width is not a
/// multiple of eight.
fn pack_samples(page: &Bitmap, black_is_1: bool) -> Vec<u8> {
    if black_is_1 {
        page.pack_rows()
    } else {
        page.to_pdf_samples()
    }
}

/// Reads `/Columns` (ISO 32000-1 Table 11, default 1728) as a usable row width.
///
/// Zero is refused rather than clamped: a row of no pixels is not a narrow
/// image. So is a width past [`MAX_IMAGE_PIXELS`], which could not hold even one
/// row — left to the decoder it would yield an image of no rows at all, which
/// reads downstream as a blank page rather than as the refusal it is.
fn columns_parm(parms: Option<&Dict>) -> Result<u32> {
    let stated = int_parm(parms, "Columns", 1728);
    match u32::try_from(stated) {
        Ok(columns) if columns >= 1 && u64::from(columns) <= MAX_IMAGE_PIXELS => Ok(columns),
        _ => Err(Error::Decode(format!(
            "CCITTFaxDecode /Columns {stated} is not a usable row width"
        ))),
    }
}

/// Reads `/Rows` (ISO 32000-1 Table 11, default 0) as a row count, where 0
/// means the count is to be taken from the data.
///
/// A negative count describes nothing and is refused; so is one past
/// [`MAX_IMAGE_PIXELS`], which no width could accompany.
fn rows_parm(parms: Option<&Dict>) -> Result<u32> {
    let stated = int_parm(parms, "Rows", 0);
    match u32::try_from(stated) {
        Ok(rows) if u64::from(rows) <= MAX_IMAGE_PIXELS => Ok(rows),
        _ => Err(Error::Decode(format!(
            "CCITTFaxDecode /Rows {stated} is not a usable row count"
        ))),
    }
}
