//! LZWDecode: 9- to 12-bit variable codes, clear code 256, EOD 257,
//! `/EarlyChange` defaulting to 1, followed by an optional predictor
//! post-pass.

use crate::error::{Error, Result};
use crate::filters::{int_parm, predictor, MAX_DECODED_LEN};
use crate::object::Dict;

const CLEAR: usize = 256;
const EOD: usize = 257;
const FIRST_FREE: usize = 258;
const MAX_TABLE: usize = 4096;
const MIN_WIDTH: u32 = 9;
const MAX_WIDTH: u32 = 12;

/// Reads big-endian (MSB-first) bit fields out of a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Reads `width` bits, or `None` when fewer bits remain.
    fn read(&mut self, width: u32) -> Option<u32> {
        let end = self.pos.checked_add(width as usize)?;
        if end > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u32;
        for i in self.pos..end {
            let bit = (self.data[i / 8] >> (7 - (i % 8))) & 1;
            value = (value << 1) | u32::from(bit);
        }
        self.pos = end;
        Some(value)
    }
}

/// The initial code table: 256 single-byte entries plus placeholders for
/// the clear (256) and end-of-data (257) codes, which are never looked up.
fn base_table() -> Vec<Vec<u8>> {
    let mut table = Vec::with_capacity(MAX_TABLE);
    for b in 0..=255u8 {
        table.push(vec![b]);
    }
    table.push(Vec::new()); // 256: clear
    table.push(Vec::new()); // 257: end of data
    table
}

/// Decodes LZWDecode data; `parms` is the resolved `/DecodeParms`
/// dictionary, if any (`/EarlyChange` and predictor parameters).
///
/// Codes start at 9 bits and grow to at most 12; with `/EarlyChange` 1
/// (the default) the width increases one code early, i.e. when the table
/// reaches 511/1023/2047 entries instead of 512/1024/2048. Truncated or
/// corrupt input leniently yields the prefix decoded so far.
pub fn decode(data: &[u8], parms: Option<&Dict>) -> Result<Vec<u8>> {
    let early: usize = if int_parm(parms, "EarlyChange", 1) == 0 {
        0
    } else {
        1
    };
    let mut out = Vec::new();
    let mut bits = BitReader::new(data);
    let mut table = base_table();
    let mut width = MIN_WIDTH;
    let mut prev: Option<usize> = None;
    while let Some(code) = bits.read(width) {
        if out.len() > MAX_DECODED_LEN {
            return Err(Error::Decode(
                "lzw: decoded stream exceeds size limit".into(),
            ));
        }
        let code = code as usize;
        if code == CLEAR {
            table.truncate(FIRST_FREE);
            width = MIN_WIDTH;
            prev = None;
            continue;
        }
        if code == EOD {
            break;
        }
        if code < table.len() {
            // Known code: emit its entry; the pending table slot becomes
            // previous entry + first byte of this one.
            if let Some(p) = prev {
                if table.len() < MAX_TABLE {
                    let mut entry = table[p].clone();
                    entry.push(table[code][0]);
                    table.push(entry);
                }
            }
            out.extend_from_slice(&table[code]);
            prev = Some(code);
        } else if code == table.len() && prev.is_some() && table.len() < MAX_TABLE {
            // The code the encoder just defined: previous entry plus its
            // own first byte.
            let p = prev.unwrap_or_default();
            let mut entry = table[p].clone();
            entry.push(entry[0]);
            out.extend_from_slice(&entry);
            table.push(entry);
            prev = Some(code);
        } else {
            // Corrupt code: keep whatever decoded so far (lenient).
            break;
        }
        if width < MAX_WIDTH && table.len() + early >= (1 << width) {
            width += 1;
        }
    }
    predictor::post_pass(out, parms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, Object};

    /// Writes big-endian (MSB-first) bit fields, mirroring `BitReader`.
    struct BitWriter {
        bytes: Vec<u8>,
        bit: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            BitWriter {
                bytes: Vec::new(),
                bit: 0,
            }
        }

        fn write(&mut self, value: u32, width: u32) {
            for k in (0..width).rev() {
                if self.bit == 0 {
                    self.bytes.push(0);
                }
                let b = ((value >> k) & 1) as u8;
                *self.bytes.last_mut().unwrap() |= b << (7 - self.bit);
                self.bit = (self.bit + 1) % 8;
            }
        }

        fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }

    /// Builds a valid LZW code stream that encodes `bytes` as literal codes
    /// only, tracking the decoder's table growth (one entry per code after
    /// the first) so the code-width schedule matches for the given
    /// EarlyChange value.
    fn encode_literals(bytes: &[u8], early: usize) -> Vec<u8> {
        let mut w = BitWriter::new();
        let mut width = MIN_WIDTH;
        w.write(CLEAR as u32, width);
        let mut table_len = FIRST_FREE;
        let mut first = true;
        for &b in bytes {
            w.write(u32::from(b), width);
            if first {
                first = false;
            } else {
                table_len += 1;
            }
            if width < MAX_WIDTH && table_len + early >= (1 << width) {
                width += 1;
            }
        }
        w.write(EOD as u32, width);
        w.finish()
    }

    fn early_parms(v: i64) -> Dict {
        let mut d = Dict::new();
        d.insert(Name("EarlyChange".into()), Object::Int(v));
        d
    }

    #[test]
    fn handmade_byte_vector_decodes() {
        // 9-bit codes 256 (clear), 65 'A', 66 'B', 258 "AB", 257 (EOD),
        // packed MSB-first by hand.
        let data = [0x80u8, 0x10, 0x48, 0x50, 0x28, 0x08];
        assert_eq!(decode(&data, None).unwrap(), b"ABAB");
    }

    #[test]
    fn kwkwk_code_just_beyond_table() {
        // 256, 65 'A', 258 (defined by this very code: "AA"), 257.
        let mut w = BitWriter::new();
        for code in [256u32, 65, 258, 257] {
            w.write(code, 9);
        }
        assert_eq!(decode(&w.finish(), None).unwrap(), b"AAA");
    }

    #[test]
    fn clear_code_resets_table_and_width() {
        let mut w = BitWriter::new();
        for code in [256u32, 65, 66, 256, 66, 65, 257] {
            w.write(code, 9);
        }
        assert_eq!(decode(&w.finish(), None).unwrap(), b"ABBA");
    }

    #[test]
    fn codes_after_eod_are_ignored() {
        let mut w = BitWriter::new();
        for code in [256u32, 72, 73, 257, 74, 75] {
            w.write(code, 9);
        }
        assert_eq!(decode(&w.finish(), None).unwrap(), b"HI");
    }

    #[test]
    fn truncated_stream_returns_prefix() {
        // First three bytes of the handmade vector hold codes 256 and 65
        // plus six spare bits: decoding stops after "A".
        let data = [0x80u8, 0x10, 0x48];
        assert_eq!(decode(&data, None).unwrap(), b"A");
    }

    #[test]
    fn missing_eod_returns_everything_decoded() {
        let mut w = BitWriter::new();
        for code in [256u32, 88, 89] {
            w.write(code, 9);
        }
        assert_eq!(decode(&w.finish(), None).unwrap(), b"XY");
    }

    #[test]
    fn width_grows_at_511_with_early_change_one() {
        // 254 literals make the table 511 entries big; with EarlyChange 1
        // the EOD after them must be written at 10 bits.
        let bytes = vec![0u8; 254];
        let stream1 = encode_literals(&bytes, 1);
        assert_eq!(decode(&stream1, None).unwrap(), bytes);
        // The same content with EarlyChange 0 keeps 9-bit codes longer and
        // must therefore pack differently.
        let stream0 = encode_literals(&bytes, 0);
        assert_ne!(stream0, stream1);
        assert_eq!(decode(&stream0, Some(&early_parms(0))).unwrap(), bytes);
    }

    #[test]
    fn early_change_mismatch_desynchronises() {
        let bytes: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        let stream0 = encode_literals(&bytes, 0);
        // Decoding an EarlyChange-0 stream with the default (1) desyncs
        // after the 511-entry boundary.
        assert_ne!(decode(&stream0, None).unwrap(), bytes);
        assert_eq!(decode(&stream0, Some(&early_parms(0))).unwrap(), bytes);
    }

    #[test]
    fn width_growth_across_511_1023_and_2047() {
        // 2500 literal codes push the table past all three growth points
        // (511, 1023, 2047) for both EarlyChange settings.
        let bytes: Vec<u8> = (0..2500u32).map(|i| (i % 251) as u8).collect();
        for early in [0i64, 1] {
            let stream = encode_literals(&bytes, early as usize);
            let parms = early_parms(early);
            assert_eq!(
                decode(&stream, Some(&parms)).unwrap(),
                bytes,
                "EarlyChange {early}"
            );
        }
    }

    #[test]
    fn default_early_change_is_one() {
        let bytes = vec![7u8; 300];
        let stream = encode_literals(&bytes, 1);
        assert_eq!(decode(&stream, None).unwrap(), bytes);
        assert_eq!(decode(&stream, Some(&early_parms(1))).unwrap(), bytes);
    }

    #[test]
    fn corrupt_code_keeps_prefix() {
        let mut w = BitWriter::new();
        // Code 300 is far beyond the table right after a clear.
        for code in [256u32, 65, 300, 66, 257] {
            w.write(code, 9);
        }
        assert_eq!(decode(&w.finish(), None).unwrap(), b"A");
    }

    #[test]
    fn empty_input_decodes_to_empty() {
        assert!(decode(&[], None).unwrap().is_empty());
    }

    #[test]
    fn predictor_post_pass_applies() {
        let diffed = [5u8, 2, 2, 2];
        let stream = encode_literals(&diffed, 1);
        let mut parms = Dict::new();
        parms.insert(Name("Predictor".into()), Object::Int(2));
        parms.insert(Name("Columns".into()), Object::Int(4));
        assert_eq!(decode(&stream, Some(&parms)).unwrap(), vec![5, 7, 9, 11]);
    }

    #[test]
    fn decompression_bomb_is_rejected() {
        // Grow the table with "just defined" (kwkwk) codes so each entry is
        // one byte longer than the last, then replay the longest entry
        // (~3.8 KiB per 12-bit code) until the output cap must trip.
        let mut w = BitWriter::new();
        let mut width = MIN_WIDTH;
        w.write(CLEAR as u32, width);
        w.write(0, width);
        let mut table_len = FIRST_FREE;
        while table_len < MAX_TABLE {
            w.write(table_len as u32, width);
            table_len += 1;
            // Mirror the decoder's EarlyChange=1 width schedule.
            if width < MAX_WIDTH && table_len + 1 >= (1 << width) {
                width += 1;
            }
        }
        let longest = MAX_TABLE - 257; // entry n decodes to n - 256 bytes
        let replays = MAX_DECODED_LEN / longest + 2;
        for _ in 0..replays {
            w.write((MAX_TABLE - 1) as u32, width);
        }
        assert!(decode(&w.finish(), None).is_err());
    }
}
