//! JBIG2 (ISO/IEC 14492 / ITU-T T.88) decoding.
//!
//! Implemented from the published standard. The entry point is the
//! `JBIG2Decode` arm of [`crate::filters::decode_stream`], which returns
//! packed 1-bit-per-pixel rows so the image layer can treat the result
//! exactly like any other `/BitsPerComponent 1` `/DeviceGray` sample data.
//!
//! Layering, bottom-up: [`mq`] is the binary arithmetic decoder (Annex E);
//! [`arith_int`] builds the integer procedures on top of it (Annex A);
//! [`bitmap`] is the bilevel pixel buffer every region decodes into;
//! [`reader`] is the bounds-checked byte cursor the header parsers run on; and
//! [`segment`] splits a PDF-embedded stream into the segments of clause 7.

#![allow(dead_code)] // Consumed by the segment layer, which lands next.

pub(crate) mod arith_int;
pub(crate) mod bitmap;
pub(crate) mod generic;
pub(crate) mod mq;
pub(crate) mod reader;
pub(crate) mod segment;

/// A decoding failure inside the JBIG2 codec.
///
/// These surface to callers as [`crate::error::Error::Decode`]. The variants
/// exist so tests can assert on the *kind* of failure without matching on
/// message text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Jbig2Error {
    /// A field ran past the end of the available bytes.
    Truncated,
    /// A field held a value the standard forbids.
    Malformed(&'static str),
    /// A construct this build does not yet decode.
    Unimplemented(&'static str),
    /// A declared bitmap exceeds [`bitmap::MAX_PIXELS`].
    TooLarge { width: u32, height: u32 },
}

impl core::fmt::Display for Jbig2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "JBIG2 stream truncated"),
            Self::Malformed(what) => write!(f, "malformed JBIG2 stream: {what}"),
            Self::Unimplemented(what) => write!(f, "unsupported JBIG2 feature: {what}"),
            Self::TooLarge { width, height } => {
                write!(f, "JBIG2 bitmap too large: {width} x {height}")
            }
        }
    }
}

impl From<Jbig2Error> for crate::error::Error {
    fn from(err: Jbig2Error) -> Self {
        crate::error::Error::Decode(err.to_string())
    }
}

/// Drives the arithmetic layers over arbitrary bytes, for the robustness
/// sweep below.
///
/// The segment layer is what will eventually feed these decoders, and it does
/// not exist yet, so there is no reachable path from `decode_stream` to assert
/// against. This hook stands in for it: it interleaves three integer
/// procedures and a symbol-ID decode against one [`mq::MqDecoder`], which is
/// the shape every region decoder has — several procedures drawing from a
/// single arithmetic stream, each adapting its own contexts.
///
/// Nothing is asserted about the values. The property under test is that the
/// call returns at all: any byte string, of any length, must decode to *some*
/// sequence of integers without panicking, hanging, or reading out of bounds,
/// because these bytes come from a PDF and a PDF is attacker-controlled. The
/// marker convention of T.88 E.3.4 is what makes that true past the end of the
/// buffer.
///
/// A fixed round count is deliberately the weaker check. It cannot see a
/// braid that keeps returning values forever, because it stops counting first;
/// `the_braid_runs_out_of_data` below is the one that runs to the end.
#[cfg(test)]
pub(crate) fn exercise_arithmetic_layers(data: &[u8], rounds: usize) {
    use std::hint::black_box;

    let mut dec = mq::MqDecoder::new(data);
    let mut set = arith_int::IntCtxSet::new();
    let mut iaid = arith_int::IaidCtx::new(8);
    for _ in 0..rounds {
        black_box(arith_int::decode_int(&mut dec, &mut set.iadh));
        black_box(arith_int::decode_int(&mut dec, &mut set.iadw));
        black_box(arith_int::decode_int(&mut dec, &mut set.iads));
        black_box(arith_int::decode_iaid(&mut dec, &mut iaid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random sweep: no input, however malformed, may
    /// panic, hang, or read out of bounds. The generator is a fixed linear
    /// congruential sequence so a failure reproduces exactly from the case
    /// number alone.
    #[test]
    fn arithmetic_layers_survive_arbitrary_input() {
        let mut state: u32 = 0x1234_5678;
        for case in 0u32..2_000 {
            let len = (state % 257) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            exercise_arithmetic_layers(&data, 64);
            state = state.wrapping_add(case);
        }
    }

    /// The braid a region decoder actually runs — several integer procedures
    /// and a symbol ID drawing from one arithmetic stream — reaches the end of
    /// its data, whatever the data is.
    ///
    /// The sweeps above stop after a fixed number of rounds, which is exactly
    /// how a decoder that never stops on its own goes unnoticed: past the end
    /// of the input the arithmetic layer cycles, and the values the braid
    /// reads out of that cycle repeat without ever including a terminator. A
    /// symbol dictionary looping on those would never finish its height class.
    /// Here the loop condition is the decoder's own, so a braid that cannot
    /// finish trips the limit instead of being cut short by it.
    #[test]
    fn the_braid_runs_out_of_data() {
        let mut state: u32 = 0x2B7E_1516;
        for case in 0u32..600 {
            let len = (state % 33) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();

            let mut dec = mq::MqDecoder::new(&data);
            let mut set = arith_int::IntCtxSet::new();
            let mut iaid = arith_int::IaidCtx::new(8);
            let mut rounds = 0usize;
            while !dec.is_exhausted() {
                let _ = arith_int::decode_int(&mut dec, &mut set.iadh);
                let _ = arith_int::decode_int(&mut dec, &mut set.iadw);
                let _ = arith_int::decode_int(&mut dec, &mut set.iads);
                let _ = arith_int::decode_iaid(&mut dec, &mut iaid);
                rounds += 1;
                assert!(rounds < 100_000, "case {case} never ran out of data");
            }
            // The height-class loop of 6.5.5 ends here and stays ended.
            for _ in 0..64 {
                assert_eq!(arith_int::decode_int(&mut dec, &mut set.iadw), None);
                let _ = arith_int::decode_iaid(&mut dec, &mut iaid);
            }
            state = state.wrapping_add(case);
        }
    }

    /// Degenerate inputs the sweep is unlikely to generate.
    ///
    /// `FF 90` and `FF 8F` straddle the marker test of T.88 E.3.4: `0x90` is
    /// above the `0x8F` threshold and takes the marker path, which leaves `BP`
    /// where it is and stuffs `0xFF00` into `C`, while `0x8F` is the largest
    /// byte that still advances `BP`. An off-by-one in that comparison shows up
    /// here and almost nowhere else.
    #[test]
    fn arithmetic_layers_survive_degenerate_input() {
        for data in [
            vec![],
            vec![0x00],
            vec![0xFF],
            vec![0xFF, 0xFF, 0xFF, 0xFF],
            vec![0xFF, 0x90],
            vec![0xFF, 0x8F],
            vec![0x00; 4096],
            vec![0xFF; 4096],
        ] {
            exercise_arithmetic_layers(&data, 256);
        }
    }
}
