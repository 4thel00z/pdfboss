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

// Only the bit reader's own tests read from it so far; the run-length code
// tables that will are the next layer up.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod bits;
