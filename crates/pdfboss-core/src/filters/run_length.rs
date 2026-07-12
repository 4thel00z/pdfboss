//! RunLengthDecode: length byte 0-127 copies the next n+1 bytes literally,
//! 129-255 repeats the next byte 257-n times, 128 is end of data.

use crate::error::{Error, Result};
use crate::filters::MAX_DECODED_LEN;

/// Decodes RunLengthDecode data. Truncated runs and a missing end-of-data
/// byte are tolerated: whatever decoded so far is returned. Output larger
/// than `MAX_DECODED_LEN` (a decompression bomb) is an error.
pub fn decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if out.len() > MAX_DECODED_LEN {
            return Err(Error::Decode(
                "run-length: decoded stream exceeds size limit".into(),
            ));
        }
        let length = data[i];
        i += 1;
        match length {
            128 => break,
            0..=127 => {
                let n = length as usize + 1;
                let end = (i + n).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            _ => {
                if i >= data.len() {
                    // Truncated repeat run: the byte to repeat is missing.
                    break;
                }
                let count = 257 - length as usize;
                let new_len = out.len() + count;
                out.resize(new_len, data[i]);
                i += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_run() {
        assert_eq!(decode(&[2, b'a', b'b', b'c', 128]).unwrap(), b"abc");
    }

    #[test]
    fn repeat_run() {
        // 254 -> 257 - 254 = 3 copies.
        assert_eq!(decode(&[254, b'x', 128]).unwrap(), b"xxx");
    }

    #[test]
    fn mixed_runs() {
        let data = [1, b'h', b'i', 253, b'!', 0, b'?', 128];
        assert_eq!(decode(&data).unwrap(), b"hi!!!!?");
    }

    #[test]
    fn boundary_lengths() {
        // 127 -> 128 literal bytes; 129 -> 128 repeats.
        let mut data = vec![127u8];
        data.extend(0..=127u8);
        data.push(129);
        data.push(b'z');
        data.push(128);
        let out = decode(&data).unwrap();
        assert_eq!(out.len(), 256);
        assert_eq!(&out[..128], (0..=127u8).collect::<Vec<_>>().as_slice());
        assert!(out[128..].iter().all(|&b| b == b'z'));
    }

    #[test]
    fn eod_stops_decoding() {
        assert_eq!(decode(&[0, b'a', 128, 0, b'b']).unwrap(), b"a");
    }

    #[test]
    fn missing_eod_is_tolerated() {
        assert_eq!(decode(&[0, b'a', 0, b'b']).unwrap(), b"ab");
    }

    #[test]
    fn truncated_literal_run_returns_prefix() {
        // Length byte promises 4 bytes but only 2 follow.
        assert_eq!(decode(&[3, b'a', b'b']).unwrap(), b"ab");
    }

    #[test]
    fn truncated_repeat_run_returns_prefix() {
        assert_eq!(decode(&[0, b'a', 250]).unwrap(), b"a");
    }

    #[test]
    fn empty_input() {
        assert!(decode(&[]).unwrap().is_empty());
        assert!(decode(&[128]).unwrap().is_empty());
    }

    #[test]
    fn decompression_bomb_is_rejected() {
        // Each `129, 0` pair expands to 128 zero bytes (the maximum 128:1
        // amplification); enough pairs must trip the output size cap
        // instead of allocating without bound.
        let pairs = MAX_DECODED_LEN / 128 + 2;
        let mut data = Vec::with_capacity(pairs * 2);
        for _ in 0..pairs {
            data.extend_from_slice(&[129, 0]);
        }
        assert!(decode(&data).is_err());
    }
}
