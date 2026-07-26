//! A test-only MQ arithmetic *encoder* (ITU-T T.88 Annex E, E.3.6 to E.3.9).
//!
//! The decoder in the parent module is checked against its own inverse. The
//! encoding procedures — INITENC, CODE0/CODE1, BYTEOUT, RENORME, SETBITS and
//! FLUSH — are a separate set of flowcharts from the decoding ones, so a bit
//! sequence coded here and read back by [`super::MqDecoder`] only survives
//! the trip if both transcriptions of Table E.1 and both interval updates
//! agree. That is the strongest check available without a conformance
//! vector, and it is what lets the integer procedures of Annex A assert the
//! *value* a chosen bit pattern denotes rather than merely that decoding
//! terminates.
//!
//! Compiled only under `cfg(test)`: nothing shipped depends on it.

use super::MqContext;

/// The MQ encoder over a growing output buffer (T.88 E.3.6).
///
/// The registers are the encoder's own: `A` is the interval, `C` the code
/// register — three bits wider than the decoder's use of it, since the
/// encoder carries into the byte already emitted — and `CT` counts the shifts
/// left before the next byte leaves.
pub(crate) struct MqEncoder {
    /// Bytes emitted so far. Element 0 is a stand-in for the byte preceding
    /// the code stream, which BYTEOUT inspects and may carry into; it is
    /// dropped by [`MqEncoder::finish`].
    out: Vec<u8>,
    /// Interval register, `A`.
    a: u32,
    /// Code register, `C`.
    c: u32,
    /// Shifts remaining before the next BYTEOUT, `CT`.
    ct: u32,
}

impl MqEncoder {
    /// INITENC (T.88 E.3.8).
    ///
    /// `CT` starts at 12 because the code register is filled 12 bits above
    /// the byte boundary; the standard's 13 applies when the byte preceding
    /// the code stream is `0xFF`, which the stand-in never is.
    pub(crate) fn new() -> Self {
        Self {
            out: vec![0x00],
            a: 0x8000,
            c: 0,
            ct: 12,
        }
    }

    /// The byte BYTEOUT is allowed to carry into, `B`.
    fn last(&self) -> u8 {
        self.out.last().copied().unwrap_or(0)
    }

    /// Emits seven bits after a `0xFF`, the stuffed-bit case of BYTEOUT.
    fn put_stuffed(&mut self) {
        self.out
            .push(u8::try_from((self.c >> 20) & 0xFF).unwrap_or(0));
        self.c &= 0x000F_FFFF;
        self.ct = 7;
    }

    /// Emits a full byte, the ordinary case of BYTEOUT. The carry bit, if
    /// any, has already been added to the previous byte, so it is dropped
    /// here by the width of the byte written.
    fn put_full(&mut self) {
        self.out
            .push(u8::try_from((self.c >> 19) & 0xFF).unwrap_or(0));
        self.c &= 0x0007_FFFF;
        self.ct = 8;
    }

    /// BYTEOUT (T.88 E.3.7): moves the completed bits of `C` into the output,
    /// propagating a carry into the byte already emitted and stuffing a
    /// seven-bit byte after any `0xFF` so that no `0xFF` is ever followed by
    /// a byte above `0x8F` inside the code stream.
    fn byte_out(&mut self) {
        if self.last() == 0xFF {
            self.put_stuffed();
        } else if self.c < 0x0800_0000 {
            self.put_full();
        } else {
            let carried = self.last().wrapping_add(1);
            if let Some(last) = self.out.last_mut() {
                *last = carried;
            }
            if carried == 0xFF {
                self.c &= 0x07FF_FFFF;
                self.put_stuffed();
            } else {
                self.put_full();
            }
        }
    }

    /// RENORME (T.88 E.3.9): shifts until `A` regains its most significant
    /// bit, emitting a byte each time `CT` runs out. Unlike the decoder's
    /// RENORMD the byte transfer follows the shift.
    fn renorm(&mut self) {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct = self.ct.saturating_sub(1);
            if self.ct == 0 {
                self.byte_out();
            }
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// CODEMPS (T.88 E.3.2): codes the more probable symbol, renormalizing
    /// and advancing the state only when `A` loses its most significant bit.
    fn code_mps(&mut self, cx: &mut MqContext) {
        let (qe, nmps, _, _) = cx.row();
        let qe = u32::from(qe);
        self.a -= qe;
        if self.a & 0x8000 == 0 {
            if self.a < qe {
                self.a = qe;
            } else {
                self.c += qe;
            }
            cx.index = nmps;
            self.renorm();
        } else {
            self.c += qe;
        }
    }

    /// CODELPS (T.88 E.3.2): codes the less probable symbol, which always
    /// renormalizes and always advances the state.
    fn code_lps(&mut self, cx: &mut MqContext) {
        let (qe, _, nlps, switch) = cx.row();
        let qe = u32::from(qe);
        self.a -= qe;
        if self.a < qe {
            self.c += qe;
        } else {
            self.a = qe;
        }
        if switch == 1 {
            cx.mps = 1 - cx.mps;
        }
        cx.index = nlps;
        self.renorm();
    }

    /// CODE0 / CODE1 (T.88 E.3.2): codes one bit against `cx`.
    pub(crate) fn encode(&mut self, cx: &mut MqContext, bit: u8) {
        if bit == cx.mps {
            self.code_mps(cx);
        } else {
            self.code_lps(cx);
        }
    }

    /// SETBITS (T.88 E.3.8): pushes `C` as high inside the final interval as
    /// it will go, so the trailing bits the decoder invents cannot carry it
    /// out again.
    fn set_bits(&mut self) {
        let temp = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= temp {
            self.c -= 0x8000;
        }
    }

    /// FLUSH (T.88 E.3.8): completes the code stream and returns it.
    ///
    /// The trailing `0xFF 0xAC` is the marker the standard appends; the
    /// decoder's BYTEIN recognizes it and switches to feeding 1-bits, which
    /// is how it keeps producing decisions past the end of the data.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.set_bits();
        self.c <<= self.ct;
        self.byte_out();
        self.c <<= self.ct;
        self.byte_out();
        if self.last() != 0xFF {
            self.out.push(0xFF);
        }
        self.out.push(0xAC);
        // The stand-in for the byte before the code stream must not have been
        // carried into: with A = 0x8000 and C = 0 at INITENC there is nothing
        // to carry out of the first byte.
        assert_eq!(self.out.first(), Some(&0x00), "carry escaped the buffer");
        self.out.split_off(1)
    }
}
