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

// The callers that will reach the decoder — the PDF filter and the JBIG2
// generic region — are not wired up yet, so nothing outside the tests reads
// from these modules for the moment.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod bits;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod codes;
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
