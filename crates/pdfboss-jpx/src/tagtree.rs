//! Packet-header bit reading (ITU-T T.800 B.10.1) and tag-tree decoding
//! (B.10.2) — shared by the Tier-2 packet reader.

use crate::error::{JpxError, Result};

/// MSB-first bit reader over packet-header bytes implementing the B.10.1
/// bit-stuffing rule: a byte following an emitted 0xFF carries only seven
/// payload bits (its MSB is a stuffed zero). Running out of bytes is a
/// `Malformed` error — packet headers never legitimately end early.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next unread byte.
    pos: usize,
    /// Bits still unread in `current` (0..=8).
    remaining: u8,
    /// The byte bits are currently drawn from, MSB first.
    current: u8,
    /// The previously completed byte was 0xFF, so the next byte loaded is
    /// stuffed (B.10.1).
    last_was_ff: bool,
}

impl<'a> BitReader<'a> {
    /// Starts reading at the first byte of `data`.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            remaining: 0,
            current: 0,
            last_was_ff: false,
        }
    }

    /// Reads a single bit (0 or 1).
    pub(crate) fn read_bit(&mut self) -> Result<u32> {
        if self.remaining == 0 {
            let Some(&byte) = self.data.get(self.pos) else {
                return Err(JpxError::Malformed("packet header truncated".into()));
            };
            self.pos += 1;
            if self.last_was_ff {
                // B.10.1: the byte following 0xFF carries seven payload
                // bits; its MSB is the stuffed zero.
                self.current = byte << 1;
                self.remaining = 7;
            } else {
                self.current = byte;
                self.remaining = 8;
            }
            self.last_was_ff = byte == 255;
        }
        let bit = u32::from(self.current >> 7);
        self.current <<= 1;
        self.remaining -= 1;
        Ok(bit)
    }

    /// Reads `count` bits (0..=32) MSB first, as the packet header's fixed-
    /// width fields require (e.g. the B-19 length fields).
    pub(crate) fn read_bits(&mut self, count: u32) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    /// Ends the packet header (B.10.1): discards the unread bits of the
    /// current byte and, when the last fully consumed byte was 0xFF, also
    /// consumes the following stuffed byte — the stuffed zero bit is part
    /// of the header even when 0xFF would otherwise have been its last
    /// byte.
    pub(crate) fn align(&mut self) -> Result<()> {
        self.remaining = 0;
        self.current = 0;
        if self.last_was_ff {
            // The stuffed byte after a trailing 0xFF belongs to this
            // header even when nothing of it is read (B.10.1).
            if self.pos < self.data.len() {
                self.pos += 1;
            }
            self.last_was_ff = false;
        }
        Ok(())
    }

    /// Index of the next unread byte — after [`Self::align`] this is where
    /// the packet body begins.
    pub(crate) fn byte_position(&self) -> usize {
        self.pos
    }
}

/// Tag-tree decoder (B.10.2): a 2-D array of non-negative integers coded
/// through a quad-tree of running minima. Shared by code-block inclusion
/// (B.10.4) and zero-bit-plane signalling (B.10.5); state persists across
/// packets of the same precinct (the causality rule of B.10.2).
// Internal node state is the tag-tree stage's to design; only `new` and
// `decode` are the frozen seam.
#[allow(dead_code)]
pub(crate) struct TagTree {
    /// Leaf columns (code-blocks spanned horizontally).
    width: u32,
    /// Leaf rows.
    height: u32,
}

impl TagTree {
    /// A tree over a `width x height` leaf grid, every node's current value
    /// initialized to zero (B.10.2). A zero-sized grid is legal (empty
    /// precinct ∩ band): `decode` is then never called for it.
    pub(crate) fn new(width: u32, height: u32) -> Self {
        TagTree { width, height }
    }

    /// Advances the knowledge about leaf `(x, y)` until either its value is
    /// fully determined and `< threshold` (returns `Ok(true)`) or it is
    /// known to be `>= threshold` (returns `Ok(false)`), consuming exactly
    /// the bits B.10.2 prescribes (querying the path from the level-0 root
    /// downwards; information already fixed by earlier calls is not read
    /// again).
    ///
    /// Callers: inclusion asks `decode(.., layer + 1)` (B.10.4); the
    /// zero-bit-plane count raises `threshold` until the call returns true,
    /// the value then being `threshold - 1` (B.10.5).
    pub(crate) fn decode(
        &mut self,
        reader: &mut BitReader<'_>,
        x: u32,
        y: u32,
        threshold: u32,
    ) -> Result<bool> {
        let _ = (self.width, self.height, reader, x, y, threshold);
        Err(JpxError::Unsupported("decoder scaffold"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_msb_first() {
        // 0b1010_0000 = 160: the first three bits are 1, 0, 1.
        let mut reader = BitReader::new(&[160]);
        assert_eq!(reader.read_bit().unwrap(), 1);
        assert_eq!(reader.read_bit().unwrap(), 0);
        assert_eq!(reader.read_bit().unwrap(), 1);
        // read_bits packs MSB first: remaining bits 0_0000 == 0.
        assert_eq!(reader.read_bits(5).unwrap(), 0);
    }

    #[test]
    fn bit_reader_unstuffs_after_ff() {
        // B.10.1: after an 0xFF byte the next byte carries 7 bits, its MSB
        // being a stuffed zero. Bytes: 255, then 0b0110_0000 = 96. Reading
        // 8 bits yields 255; the following SEVEN bits yield 110_0000 = 96.
        let mut reader = BitReader::new(&[255, 96]);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        assert_eq!(reader.read_bits(7).unwrap(), 96);
        // Both bytes fully consumed.
        assert_eq!(reader.byte_position(), 2);
    }

    #[test]
    fn align_discards_partial_bytes() {
        // Read 1 bit of 0b1000_0000 = 128, then align: the next read must
        // start at byte 1 (value 77).
        let mut reader = BitReader::new(&[128, 77]);
        assert_eq!(reader.read_bit().unwrap(), 1);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 1);
        assert_eq!(reader.read_bits(8).unwrap(), 77);
    }

    #[test]
    fn align_consumes_the_stuffed_byte_after_a_trailing_ff() {
        // B.10.1: "the single zero bit stuffed after a byte with 0xFF must
        // be included even if the 0xFF would otherwise have been the last
        // byte" — so a header ending in 0xFF is followed by one stuffed
        // byte that align() must swallow. Bytes: 255, 127, 77.
        let mut reader = BitReader::new(&[255, 127, 77]);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 2);
        assert_eq!(reader.read_bits(8).unwrap(), 77);
    }

    #[test]
    fn align_is_a_no_op_on_a_byte_boundary() {
        let mut reader = BitReader::new(&[10, 20]);
        assert_eq!(reader.read_bits(8).unwrap(), 10);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 1);
        assert_eq!(reader.read_bits(8).unwrap(), 20);
    }

    #[test]
    fn bit_reader_errors_on_exhaustion() {
        let mut reader = BitReader::new(&[7]);
        assert_eq!(reader.read_bits(8).unwrap(), 7);
        assert!(
            matches!(reader.read_bit(), Err(JpxError::Malformed(msg)) if msg.contains("header"))
        );
    }
}
