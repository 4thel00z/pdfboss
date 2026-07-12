//! ASCIIHexDecode: hex pairs to bytes; whitespace ignored, `>` terminates,
//! an odd trailing digit is padded with `0`.

use crate::error::Result;
use crate::filters::is_pdf_whitespace;

/// Decodes ASCIIHexDecode data. Whitespace is ignored, `>` ends the data
/// (anything after it is ignored), an odd number of digits is padded with a
/// trailing `0`, and other unexpected bytes are leniently skipped.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut high: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        let nibble = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ if is_pdf_whitespace(c) => continue,
            // Lenient: anything else is skipped.
            _ => continue,
        };
        match high.take() {
            None => high = Some(nibble),
            Some(h) => out.push(h << 4 | nibble),
        }
    }
    if let Some(h) = high {
        // Odd digit count: the final digit is the high nibble.
        out.push(h << 4);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_simple_pairs() {
        assert_eq!(decode(b"48656C6C6F>").unwrap(), b"Hello");
    }

    #[test]
    fn lower_and_mixed_case_digits() {
        assert_eq!(decode(b"48656c6C6f>").unwrap(), b"Hello");
    }

    #[test]
    fn embedded_whitespace_is_ignored() {
        assert_eq!(decode(b"48 65\t6C\r\n6C 6F\x0c>").unwrap(), b"Hello");
        assert_eq!(decode(b"4\n8656C6C6F>").unwrap(), b"Hello");
    }

    #[test]
    fn odd_length_pads_with_zero() {
        assert_eq!(decode(b"48656C6C6F7>").unwrap(), b"Hello\x70");
        assert_eq!(decode(b"7>").unwrap(), [0x70]);
    }

    #[test]
    fn terminator_stops_decoding() {
        assert_eq!(decode(b"4869>4141").unwrap(), b"Hi");
    }

    #[test]
    fn missing_terminator_decodes_to_end() {
        assert_eq!(decode(b"4869").unwrap(), b"Hi");
    }

    #[test]
    fn empty_input() {
        assert!(decode(b"").unwrap().is_empty());
        assert!(decode(b">").unwrap().is_empty());
    }

    #[test]
    fn unexpected_bytes_are_skipped() {
        assert_eq!(decode(b"48zz69!>").unwrap(), b"Hi");
    }
}
