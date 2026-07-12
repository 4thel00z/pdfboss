//! ASCII85Decode: base-85 groups of 5 chars to 4 bytes; whitespace ignored,
//! `z` is four zero bytes (only between groups), `~>` terminates, a final
//! partial group of n chars yields n-1 bytes; a leading `<~` is tolerated.

use crate::error::{Error, Result};
use crate::filters::is_pdf_whitespace;

/// Decodes ASCII85Decode data. Whitespace is ignored anywhere, `z` stands
/// for four zero bytes (allowed only between groups), `~` terminates
/// (normally as `~>`; the data simply ending is tolerated too), and a final
/// partial group of n chars yields n-1 bytes. A leading `<~` is skipped.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() / 5 * 4 + 4);
    let mut group = [0u8; 5];
    let mut have = 0usize;
    let mut i = 0usize;
    // Tolerate the encoder-style "<~" opener (after leading whitespace).
    while i < data.len() && is_pdf_whitespace(data[i]) {
        i += 1;
    }
    if data.len() >= i + 2 && data[i] == b'<' && data[i + 1] == b'~' {
        i += 2;
    }
    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            b'~' => break,
            b'z' => {
                if have != 0 {
                    return Err(Error::Decode("ascii85: 'z' inside a group".into()));
                }
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            b'!'..=b'u' => {
                group[have] = c - b'!';
                have += 1;
                if have == 5 {
                    push_group(&mut out, &group, 5)?;
                    have = 0;
                }
            }
            _ if is_pdf_whitespace(c) => {}
            // Lenient: other unexpected bytes are skipped.
            _ => {}
        }
    }
    if have >= 2 {
        // Partial final group: pad with 'u' (84) and keep n-1 bytes.
        group[have..].fill(84);
        push_group(&mut out, &group, have)?;
    }
    // A single leftover char yields n-1 = 0 bytes and is dropped.
    Ok(out)
}

/// Converts one 5-digit base-85 group into its first `n - 1` bytes.
fn push_group(out: &mut Vec<u8>, digits: &[u8; 5], n: usize) -> Result<()> {
    let mut value: u64 = 0;
    for &d in digits {
        value = value * 85 + u64::from(d);
    }
    if value > u64::from(u32::MAX) {
        return Err(Error::Decode("ascii85: group exceeds 32 bits".into()));
    }
    let bytes = (value as u32).to_be_bytes();
    out.extend_from_slice(&bytes[..n - 1]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference encoder used to build round-trip vectors.
    fn encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in data.chunks(4) {
            let mut quad = [0u8; 4];
            quad[..chunk.len()].copy_from_slice(chunk);
            let value = u32::from_be_bytes(quad);
            if value == 0 && chunk.len() == 4 {
                out.push(b'z');
                continue;
            }
            let mut digits = [0u8; 5];
            let mut v = value;
            for d in digits.iter_mut().rev() {
                *d = (v % 85) as u8 + b'!';
                v /= 85;
            }
            out.extend_from_slice(&digits[..chunk.len() + 1]);
        }
        out.extend_from_slice(b"~>");
        out
    }

    #[test]
    fn decodes_known_group() {
        assert_eq!(decode(b"9jqo^~>").unwrap(), b"Man ");
    }

    #[test]
    fn leading_marker_is_tolerated() {
        assert_eq!(decode(b"<~9jqo^~>").unwrap(), b"Man ");
        assert_eq!(decode(b"  <~9jqo^~>").unwrap(), b"Man ");
    }

    #[test]
    fn z_is_four_zero_bytes() {
        assert_eq!(decode(b"z~>").unwrap(), [0, 0, 0, 0]);
        assert_eq!(
            decode(b"9jqo^z9jqo^~>").unwrap(),
            b"Man \x00\x00\x00\x00Man "
        );
    }

    #[test]
    fn z_inside_group_is_an_error() {
        assert!(matches!(decode(b"9jz~>"), Err(Error::Decode(_))));
    }

    #[test]
    fn partial_final_groups() {
        // 4 chars -> 3 bytes, 3 chars -> 2 bytes, 2 chars -> 1 byte.
        assert_eq!(decode(b"9jqo~>").unwrap(), b"Man");
        assert_eq!(decode(b"9jq~>").unwrap(), b"Ma");
        assert_eq!(decode(b"9j~>").unwrap(), b"M");
    }

    #[test]
    fn single_trailing_char_yields_no_bytes() {
        assert_eq!(decode(b"9jqo^9~>").unwrap(), b"Man ");
    }

    #[test]
    fn whitespace_is_ignored_everywhere() {
        assert_eq!(decode(b"9j qo\r\n^~>").unwrap(), b"Man ");
        assert_eq!(decode(b"\t9jqo^\x0c~>").unwrap(), b"Man ");
    }

    #[test]
    fn missing_terminator_decodes_to_end() {
        assert_eq!(decode(b"9jqo^").unwrap(), b"Man ");
        assert_eq!(decode(b"9jqo").unwrap(), b"Man");
    }

    #[test]
    fn group_overflow_is_an_error() {
        // "uuuuu" exceeds 2^32 - 1.
        assert!(matches!(decode(b"uuuuu~>"), Err(Error::Decode(_))));
    }

    #[test]
    fn round_trips_all_partial_lengths() {
        let sample = b"\x00\x01\xfePDF-1.7 sample";
        for len in 0..=sample.len() {
            let enc = encode(&sample[..len]);
            assert_eq!(decode(&enc).unwrap(), &sample[..len], "length {len}");
        }
        // All-zero data exercises the 'z' shorthand.
        let zeros = [0u8; 9];
        assert_eq!(decode(&encode(&zeros)).unwrap(), zeros);
    }

    #[test]
    fn empty_input() {
        assert!(decode(b"").unwrap().is_empty());
        assert!(decode(b"~>").unwrap().is_empty());
        assert!(decode(b"<~~>").unwrap().is_empty());
    }
}
