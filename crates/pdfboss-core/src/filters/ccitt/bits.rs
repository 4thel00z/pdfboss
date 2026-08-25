//! An MSB-first bit reader over a borrowed byte slice.
//!
//! Facsimile coding transmits each variable-length code word most significant
//! bit first, packed against its neighbours with no padding (ITU-T T.4
//! §4.1.2), so a code word routinely straddles a byte boundary and the reader
//! that feeds the code tables has to be a bit cursor rather than a byte one.
//!
//! Two properties carry the whole design.
//!
//! **Peeking is separate from consuming.** Prefix-code lookup works by
//! examining the widest code the table holds, matching it, and then consuming
//! only the bits the matched code actually occupies. [`BitReader::peek`]
//! therefore leaves the position alone, and past the end of the data it
//! returns zero bits instead of failing: a valid final code may sit in the
//! last byte with padding after it, and a lookup that had to check for the end
//! before every bit would be neither total nor fast.
//!
//! **Consuming past the end is visible.** Zero-filled peeks alone would let a
//! truncated stream be read forever, so [`BitReader::read_bit`] yields `None`
//! and [`BitReader::is_exhausted`] reports true once the position reaches the
//! end of the data. That is what a decoding loop tests to stop.

/// A cursor over a borrowed byte slice, positioned between two bits.
///
/// The position is counted **in bits** from the start of the data. Holding a
/// byte index and a bit offset instead is the version that gets a peek
/// spanning a byte boundary wrong, so there is deliberately only one number
/// here.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`, never past
    /// `8 * data.len()`.
    pos: usize,
}

/// The widest peek this reader will assemble.
///
/// The longest code word in T.4 is 13 bits and the longest pattern any caller
/// matches is shorter still, so a request wider than a machine word is a
/// caller error rather than a stream one; clamping keeps the shift in
/// [`BitReader::peek`] in range without a branch per bit.
const MAX_PEEK: u32 = 32;

impl<'a> BitReader<'a> {
    /// A reader positioned at the first bit of `data`.
    pub(crate) fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0 }
    }

    /// The number of bits in the data.
    ///
    /// `saturating_mul` rather than `*`: a slice long enough to overflow this
    /// cannot be allocated, but the cursor's invariants should not depend on
    /// that being true.
    fn end(&self) -> usize {
        self.data.len().saturating_mul(8)
    }

    /// The bit at absolute position `index`, or 0 past the end of the data.
    ///
    /// Zero-filling here, once, is what makes every caller total.
    fn bit_at(&self, index: usize) -> u32 {
        match self.data.get(index / 8) {
            Some(byte) => u32::from((byte >> (7 - (index % 8))) & 1),
            None => 0,
        }
    }

    /// The next `n` bits as an integer, most significant first, **without
    /// consuming them**.
    ///
    /// Bits past the end of the data read as zero, so the result is defined
    /// for every position and every width. `n` of 0 yields 0; `n` above
    /// [`MAX_PEEK`] is clamped.
    pub(crate) fn peek(&self, n: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..n.min(MAX_PEEK) {
            value = (value << 1) | self.bit_at(self.pos.saturating_add(i as usize));
        }
        value
    }

    /// The next bit, advancing the position, or `None` at the end of the data.
    ///
    /// This is the read that a truncated stream fails, which is why the mode
    /// bit of a mixed-coding row is read with it rather than peeked.
    pub(crate) fn read_bit(&mut self) -> Option<u8> {
        if self.is_exhausted() {
            return None;
        }
        let bit = self.bit_at(self.pos);
        self.pos += 1;
        Some(bit as u8)
    }

    /// Advances by `n` bits, stopping at the end of the data.
    ///
    /// Stopping rather than running on keeps [`BitReader::bit_pos`] a position
    /// within the stream, which is what byte alignment and any
    /// bytes-consumed arithmetic need it to be. A caller that has to know
    /// whether the stream ran out asks [`BitReader::is_exhausted`].
    pub(crate) fn skip(&mut self, n: u32) {
        self.pos = self.pos.saturating_add(n as usize).min(self.end());
    }

    /// Advances to the next byte boundary, or stays put if already on one.
    ///
    /// This is `/EncodedByteAlign` (ISO 32000-1 Table 11): the rows of such a
    /// stream each begin on a byte boundary, and the fill bits between them
    /// carry nothing.
    pub(crate) fn align_to_byte(&mut self) {
        let offset = self.pos % 8;
        if offset != 0 {
            // Clamping again rather than trusting `end()` to be a multiple of
            // 8: it is, for every slice that can exist, but the cursor's
            // invariant should not rest on an allocation limit.
            self.pos = self.pos.saturating_add(8 - offset).min(self.end());
        }
    }

    /// The next `n` whole bytes as a slice, advancing past them.
    ///
    /// `None` when the position is not on a byte boundary or fewer than `n`
    /// bytes remain. The byte-boundary requirement is the caller's to meet —
    /// JBIG2 embeds byte-counted fields only after an explicit alignment
    /// (T.88 6.4.11 step 5), so an unaligned call is a field read out of the
    /// wrong place, not a slice to invent.
    pub(crate) fn take_aligned_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if !self.pos.is_multiple_of(8) {
            return None;
        }
        let start = self.pos / 8;
        let end = start.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        self.pos = end.saturating_mul(8);
        Some(&self.data[start..end])
    }

    /// Whether every bit of the data has been consumed.
    pub(crate) fn is_exhausted(&self) -> bool {
        self.pos >= self.end()
    }

    /// How many bits are left unread.
    ///
    /// This is what distinguishes a stream that stopped from a stream that is
    /// wrong. Because [`BitReader::peek`] zero-fills, a lookup that fails with
    /// fewer bits left than the widest code could have succeeded had the data
    /// continued — so the failure is the end of the data, not corruption — while
    /// a lookup that fails with a full window in hand read something that is
    /// genuinely not a code.
    pub(crate) fn remaining(&self) -> usize {
        self.end().saturating_sub(self.pos)
    }

    /// The current position, in bits from the start of the data.
    pub(crate) fn bit_pos(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_most_significant_first() {
        let mut r = BitReader::new(&[0b1011_0010, 0b0100_0000]);
        let got: Vec<u8> = (0..10).filter_map(|_| r.read_bit()).collect();
        assert_eq!(got, vec![1, 0, 1, 1, 0, 0, 1, 0, 0, 1]);
    }

    #[test]
    fn peek_does_not_consume_and_spans_bytes() {
        let mut r = BitReader::new(&[0b1010_1010, 0b1100_0000]);
        assert_eq!(r.peek(4), 0b1010);
        assert_eq!(r.peek(4), 0b1010, "peek must not advance");
        assert_eq!(r.bit_pos(), 0);
        r.skip(6);
        assert_eq!(r.peek(4), 0b1011, "two bits from each byte");
        assert_eq!(r.bit_pos(), 6);
    }

    /// Past the end, peek reads zeros. That is what makes the code-table
    /// lookup total: a truncated stream matches some short code or none, and
    /// either way it never indexes outside the buffer.
    #[test]
    fn peek_past_the_end_is_zero_filled() {
        let mut r = BitReader::new(&[0xFF]);
        r.skip(4);
        assert_eq!(r.peek(8), 0b1111_0000);
        r.skip(8);
        assert_eq!(r.peek(13), 0);
        assert!(r.is_exhausted());
        assert_eq!(r.read_bit(), None);
    }

    #[test]
    fn peek_of_zero_and_of_the_maximum_width() {
        // `peek` takes the reader by shared reference, because a peek that
        // could consume would defeat its purpose.
        let r = BitReader::new(&[0xAB, 0xCD]);
        assert_eq!(r.peek(0), 0);
        assert_eq!(r.peek(16), 0xABCD);
        assert_eq!(r.bit_pos(), 0, "neither peek consumed anything");
    }

    #[test]
    fn align_to_byte_is_a_no_op_when_already_aligned() {
        let mut r = BitReader::new(&[0x0F, 0xF0]);
        r.align_to_byte();
        assert_eq!(r.bit_pos(), 0);
        r.skip(1);
        r.align_to_byte();
        assert_eq!(r.bit_pos(), 8);
        r.align_to_byte();
        assert_eq!(r.bit_pos(), 8);
    }

    /// Alignment discards the rest of the current byte rather than a whole
    /// byte: a reader one bit into a byte lands on the next byte, and the
    /// byte it lands on is still unread.
    #[test]
    fn align_to_byte_discards_only_the_remainder_of_the_current_byte() {
        let mut r = BitReader::new(&[0b1000_0000, 0b0101_0101]);
        assert_eq!(r.read_bit(), Some(1));
        r.align_to_byte();
        assert_eq!(r.peek(8), 0b0101_0101);
    }

    #[test]
    fn skipping_past_the_end_saturates_rather_than_overflowing() {
        let mut r = BitReader::new(&[0x00]);
        r.skip(u32::MAX);
        assert!(r.is_exhausted());
        // The position stays a position: it stops at the end of the data
        // instead of running away with the skip count.
        assert_eq!(r.bit_pos(), 8);
        r.skip(u32::MAX);
        assert!(r.is_exhausted());
        assert_eq!(r.bit_pos(), 8);
        assert_eq!(r.peek(8), 0);
        r.align_to_byte();
        assert_eq!(r.bit_pos(), 8);
    }

    /// A code may sit in the last byte with padding after it, so reaching the
    /// final bit must not be an error — but the read *after* it must be, or a
    /// truncated stream never terminates a decoding loop.
    #[test]
    fn the_last_bit_reads_and_the_one_after_it_does_not() {
        let mut r = BitReader::new(&[0b0000_0001]);
        r.skip(7);
        assert!(!r.is_exhausted());
        assert_eq!(r.read_bit(), Some(1));
        assert!(r.is_exhausted());
        assert_eq!(r.read_bit(), None);
        assert_eq!(r.bit_pos(), 8);
    }

    #[test]
    fn remaining_counts_down_to_zero_and_stops() {
        let mut r = BitReader::new(&[0x00, 0x00]);
        assert_eq!(r.remaining(), 16);
        r.skip(5);
        assert_eq!(r.remaining(), 11);
        r.skip(u32::MAX);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn take_aligned_bytes_requires_alignment_and_enough_data() {
        let mut r = BitReader::new(&[0xAB, 0xCD, 0xEF]);
        r.skip(3);
        assert_eq!(r.take_aligned_bytes(1), None, "unaligned");
        r.align_to_byte();
        assert_eq!(r.take_aligned_bytes(3), None, "only two bytes remain");
        assert_eq!(r.take_aligned_bytes(1), Some(&[0xCD][..]));
        assert_eq!(r.bit_pos(), 16, "the cursor moved past the byte");
        assert_eq!(r.take_aligned_bytes(0), Some(&[][..]));
        assert_eq!(r.take_aligned_bytes(1), Some(&[0xEF][..]));
        assert!(r.is_exhausted());
        assert_eq!(r.take_aligned_bytes(1), None);
    }

    #[test]
    fn an_empty_buffer_is_immediately_exhausted() {
        let mut r = BitReader::new(&[]);
        assert!(r.is_exhausted());
        assert_eq!(r.read_bit(), None);
        assert_eq!(r.peek(13), 0);
        assert_eq!(r.bit_pos(), 0);
    }
}
