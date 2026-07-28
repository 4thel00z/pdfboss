//! A bounds-checked big-endian byte reader.
//!
//! Every multi-byte integer in T.88 is big-endian (7.2 for segment headers,
//! 7.4 for the region fields), and every one of them is read out of a PDF
//! stream this decoder does not control. This reader is the single place that
//! turns "past the end" into an error instead of a panic.
//!
//! One rule governs the whole type: **a failed read leaves the cursor
//! untouched**. A caller that probes a field and finds it short can therefore
//! recover, and the long form of the referred-to count field in T.88 7.2.4 can
//! step back over the byte it just read and take it again as the first of four.

use super::Jbig2Error;

/// A cursor over a borrowed byte slice.
///
/// The lifetime is threaded through [`Reader::take`] and [`Reader::rest`] so
/// segment data can be borrowed straight out of the input rather than copied.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader positioned at the start of `data`.
    pub(crate) fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }

    /// The next `n` bytes, advancing the cursor.
    ///
    /// `checked_add` matters: a length field read from the stream can be
    /// `usize::MAX`, and `self.pos + n` would wrap to something in range.
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Jbig2Error> {
        let end = self.pos.checked_add(n).ok_or(Jbig2Error::Truncated)?;
        let slice = self.data.get(self.pos..end).ok_or(Jbig2Error::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// One byte.
    pub(crate) fn u8(&mut self) -> Result<u8, Jbig2Error> {
        let bytes = self.take(1)?;
        match bytes {
            [b] => Ok(*b),
            _ => Err(Jbig2Error::Truncated),
        }
    }

    /// One byte, read as a two's-complement signed value.
    ///
    /// The AT pixel offsets of T.88 7.4.6.3 are signed bytes.
    pub(crate) fn i8(&mut self) -> Result<i8, Jbig2Error> {
        Ok(self.u8()? as i8)
    }

    /// A two-byte big-endian integer.
    pub(crate) fn u16(&mut self) -> Result<u16, Jbig2Error> {
        let bytes = self.take(2)?;
        match bytes {
            [hi, lo] => Ok(u16::from_be_bytes([*hi, *lo])),
            _ => Err(Jbig2Error::Truncated),
        }
    }

    /// A four-byte big-endian integer.
    pub(crate) fn u32(&mut self) -> Result<u32, Jbig2Error> {
        let bytes = self.take(4)?;
        match bytes {
            [a, b, c, d] => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
            _ => Err(Jbig2Error::Truncated),
        }
    }

    /// Moves the cursor back `n` bytes.
    ///
    /// Only the referred-to count field of T.88 7.2.4 needs this: its short
    /// form is one byte whose top three bits hold the count, and the value 7
    /// means that same byte is instead the first of a four-byte field. Rather
    /// than peek, the parser reads the byte and rewinds.
    ///
    /// Rewinding past the start is an error, and like every other failed
    /// operation it leaves the cursor where it was.
    pub(crate) fn seek_back(&mut self, n: usize) -> Result<(), Jbig2Error> {
        self.pos = self.pos.checked_sub(n).ok_or(Jbig2Error::Truncated)?;
        Ok(())
    }

    /// Everything from the cursor to the end, without advancing.
    pub(crate) fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }

    /// The cursor's offset from the start of the input.
    #[allow(dead_code)] // Reached from the fixture builders only.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// How many bytes remain unread.
    pub(crate) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Whether the cursor has reached the end of the input.
    pub(crate) fn is_empty(&self) -> bool {
        self.remaining() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::Jbig2Error;

    #[test]
    fn reads_big_endian_fields_in_order() {
        let data = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0xFF];
        let mut r = Reader::new(&data);
        assert_eq!(r.u32(), Ok(0x0102_0304));
        assert_eq!(r.u16(), Ok(0x0506));
        assert_eq!(r.i8(), Ok(-1));
        assert_eq!(r.remaining(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn underrun_is_truncated_not_a_panic() {
        let mut r = Reader::new(&[0x01, 0x02]);
        assert_eq!(r.u32(), Err(Jbig2Error::Truncated));
        // A failed read must not consume anything.
        assert_eq!(r.pos(), 0);
        assert_eq!(r.u16(), Ok(0x0102));
        assert_eq!(r.u8(), Err(Jbig2Error::Truncated));
    }

    #[test]
    fn take_returns_a_borrowed_slice_and_advances() {
        let data = [1u8, 2, 3, 4];
        let mut r = Reader::new(&data);
        assert_eq!(r.take(3), Ok(&data[..3]));
        assert_eq!(r.pos(), 3);
        assert_eq!(r.take(2), Err(Jbig2Error::Truncated));
        assert_eq!(r.rest(), &[4]);
    }

    #[test]
    fn take_of_zero_is_allowed_at_the_end() {
        let mut r = Reader::new(&[]);
        assert_eq!(r.take(0), Ok(&[][..]));
        assert_eq!(r.take(usize::MAX), Err(Jbig2Error::Truncated));
    }

    /// The long form of the referred-to count field re-reads its own first
    /// byte as the first of four, so the cursor has to be able to step back.
    #[test]
    fn seek_back_rewinds_and_refuses_to_pass_the_start() {
        let data = [0xE0u8, 0x00, 0x00, 0x05];
        let mut r = Reader::new(&data);
        assert_eq!(r.u8(), Ok(0xE0));
        assert_eq!(r.seek_back(1), Ok(()));
        assert_eq!(r.pos(), 0);
        assert_eq!(r.u32(), Ok(0xE000_0005));
        assert_eq!(r.seek_back(9), Err(Jbig2Error::Truncated));
        // The refused rewind left the cursor where it was.
        assert_eq!(r.pos(), 4);
    }
}
