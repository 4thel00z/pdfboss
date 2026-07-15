//! Type 1 font program decryption and segmentation (Type 1 Font Format spec
//! ch. 2 §PFB and ch. 7 `eexec`/charstring encryption; ISO 32000 §9.9
//! `FontFile`).
//!
//! A `FontFile` stream holds a Type1 program in one of two container shapes:
//! **PFB** (a sequence of `0x80`-tagged segments) or **raw** (plain
//! concatenation, split at the ASCII `eexec` token, optionally hex-encoded).
//! Either way, the program's tail is an `eexec`-encrypted "private" portion;
//! the same stream cipher, keyed differently, also protects each charstring
//! inside it once decrypted (a later task decrypts those with
//! `CHARSTRING_KEY`).
//!
//! This module only decrypts and segments a program into its clear-text
//! header and decrypted private portion; parsing those bytes into a font
//! (dictionaries, encoding, charstrings-as-outlines) is a later task's job.

// --- Type1 stream cipher constants (spec ch. 7) -----------------------------

/// The fixed key for decrypting a `FontFile`'s `eexec` portion.
const EEXEC_KEY: u16 = 55665;
/// The fixed key for decrypting an individual charstring, once extracted
/// from the decrypted `eexec` portion (consumed by a later task).
#[allow(dead_code)]
const CHARSTRING_KEY: u16 = 4330;
/// Cipher multiplier, shared by both keys' recurrences.
const C1: u16 = 52845;
/// Cipher addend, shared by both keys' recurrences.
const C2: u16 = 22719;
/// Number of scrambled lead bytes an `eexec` region always starts with.
/// (Charstrings use their own `lenIV`, conventionally also 4, read from the
/// decrypted `/Private` dict by a later task.)
const EEXEC_SKIP: usize = 4;

/// Decrypts a Type1-encrypted byte string with the stream cipher of spec
/// ch. 7: starting from `R = key`, each ciphertext byte `C` yields plaintext
/// byte `P = C ^ high_byte(R)`, after which `R` advances to
/// `(R + C) * C1 + C2` (all arithmetic wrapping `u16`, and driven by the
/// *ciphertext* byte, not the plaintext one). The cipher's first `skip`
/// emitted bytes are scrambled padding with no meaning and are dropped from
/// the result.
///
/// Returns `None` if `cipher` is shorter than `skip` -- too short to ever
/// have come from a valid `encrypt`, so there is nothing meaningful to
/// return.
fn decrypt(cipher: &[u8], key: u16, skip: usize) -> Option<Vec<u8>> {
    if cipher.len() < skip {
        return None;
    }
    let mut r = key;
    let mut plain = Vec::with_capacity(cipher.len());
    for &c in cipher {
        let p = c ^ (r >> 8) as u8;
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
        plain.push(p);
    }
    Some(plain.split_off(skip))
}

/// PostScript whitespace (spec ch. 2): space, tab, CR, LF, form feed.
fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

/// Whether `region`'s first 4 non-whitespace bytes are all ASCII hex digits
/// -- the test spec ch. 7 uses to recognize a hex-encoded (rather than raw
/// binary) `eexec` region. Fewer than 4 non-whitespace bytes available in
/// `region` cannot satisfy the check, so that case is treated as (trivially
/// malformed) binary.
fn looks_like_hex(region: &[u8]) -> bool {
    let mut seen = 0usize;
    for &b in region {
        if is_ps_whitespace(b) {
            continue;
        }
        if !b.is_ascii_hexdigit() {
            return false;
        }
        seen += 1;
        if seen == 4 {
            return true;
        }
    }
    false
}

/// Hex-decodes `region`, ignoring whitespace and stopping at the first byte
/// that is neither whitespace nor an ASCII hex digit (a hex-encoded `eexec`
/// region is conventionally wrapped across multiple lines). A trailing lone
/// nibble -- an odd count of hex digits before the stop -- is dropped rather
/// than guessed at.
fn hex_decode_lenient(region: &[u8]) -> Vec<u8> {
    let mut nibbles = Vec::with_capacity(region.len());
    for &b in region {
        if is_ps_whitespace(b) {
            continue;
        }
        match (b as char).to_digit(16) {
            Some(n) => nibbles.push(n as u8),
            None => break,
        }
    }
    nibbles
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

/// Splits a raw (non-PFB) Type1 program at the ASCII `eexec` token:
/// `clear_text` is everything up to and including `eexec` and the whitespace
/// immediately following it; the remainder is the (possibly hex-encoded)
/// `eexec` region, which is hex-decoded first when [`looks_like_hex`] says
/// so, then decrypted. `None` if the `eexec` token is not present anywhere in
/// `program`, or if decryption fails.
fn segment_raw(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    const TOKEN: &[u8] = b"eexec";
    let token_at = program.windows(TOKEN.len()).position(|w| w == TOKEN)?;
    let mut region_start = token_at + TOKEN.len();
    while program
        .get(region_start)
        .is_some_and(|&b| is_ps_whitespace(b))
    {
        region_start += 1;
    }
    let clear_text = program.get(..region_start)?.to_vec();
    let region = program.get(region_start..)?;

    let raw = if looks_like_hex(region) {
        hex_decode_lenient(region)
    } else {
        region.to_vec()
    };
    let priv_dec = decrypt(&raw, EEXEC_KEY, EEXEC_SKIP)?;
    Some((clear_text, priv_dec))
}

/// Walks a PFB program's `0x80 <type> <len:u32 LE>`-tagged segments (spec
/// ch. 2 §PFB), concatenating ASCII (type 1) segments into `clear_text` and
/// binary (type 2) segments into the raw `eexec` ciphertext, then decrypting
/// that ciphertext. Stops at a type-3 (EOF) segment or the first malformed
/// segment header/length -- every length is bounds-checked against what
/// remains of `program` before it is used to slice. A program with no
/// type-2 segment (nothing to decrypt) yields `None`.
fn segment_pfb(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut clear_text = Vec::new();
    let mut cipher = Vec::new();
    let mut pos = 0usize;
    while program.get(pos) == Some(&0x80) {
        let seg_type = *program.get(pos.checked_add(1)?)?;
        let len_start = pos.checked_add(2)?;
        let len_end = len_start.checked_add(4)?;
        let len = u32::from_le_bytes(program.get(len_start..len_end)?.try_into().ok()?) as usize;
        let data_start = len_end;
        let data_end = data_start.checked_add(len)?;
        let data = program.get(data_start..data_end)?;
        match seg_type {
            1 => clear_text.extend_from_slice(data),
            2 => cipher.extend_from_slice(data),
            _ => break, // type 3 (EOF), or an unrecognized/malformed type
        }
        pos = data_end;
    }
    if cipher.is_empty() {
        return None;
    }
    let priv_dec = decrypt(&cipher, EEXEC_KEY, EEXEC_SKIP)?;
    Some((clear_text, priv_dec))
}

/// Splits a `FontFile` program into its clear-text header and
/// eexec-decrypted private portion (spec ch. 2 §PFB, ch. 7; ISO 32000 §9.9
/// `FontFile`): a leading `0x80` byte marks a PFB container ([`segment_pfb`]);
/// anything else is a raw, plainly-concatenated program ([`segment_raw`]).
///
/// `None` if no `eexec` region could be found at all (a PFB program with no
/// type-2 segment, or a raw program with no `eexec` token), or if the region
/// found failed to decrypt.
fn segment(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if program.first() == Some(&0x80) {
        segment_pfb(program)
    } else {
        segment_raw(program)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The shared cipher's inverse: same recurrence, but `C` is the byte this
    /// function itself just emitted (rather than one read from ciphertext).
    /// Prepends `skip` filler bytes before encrypting, mirroring what
    /// `decrypt` then drops.
    fn encrypt(plain: &[u8], key: u16, skip: usize) -> Vec<u8> {
        let mut r = key;
        let mut out = Vec::new();
        let mut buf = vec![0u8; skip];
        buf.extend_from_slice(plain);
        for &p in &buf {
            let c = p ^ (r >> 8) as u8;
            r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
            out.push(c);
        }
        out
    }

    #[test]
    fn decrypt_round_trips_eexec() {
        let plain = b"/Private 10 dict dup begin";
        let cipher = encrypt(plain, EEXEC_KEY, EEXEC_SKIP);
        assert_eq!(
            decrypt(&cipher, EEXEC_KEY, EEXEC_SKIP).as_deref(),
            Some(&plain[..])
        );
    }

    #[test]
    fn decrypt_drops_skip_bytes_and_rejects_short_input() {
        // A ciphertext of exactly `skip` bytes decrypts to empty, not None.
        let cipher = encrypt(b"", CHARSTRING_KEY, 4);
        assert_eq!(
            decrypt(&cipher, CHARSTRING_KEY, 4).as_deref(),
            Some(&b""[..])
        );
        // Fewer than `skip` bytes -> None (can't be a valid encrypted object).
        assert_eq!(decrypt(&[1, 2, 3], CHARSTRING_KEY, 4), None);
    }

    #[test]
    fn decrypt_with_wrong_key_differs() {
        let plain = b"hello type1";
        let cipher = encrypt(plain, EEXEC_KEY, EEXEC_SKIP);
        assert_ne!(
            decrypt(&cipher, 12345, EEXEC_SKIP).as_deref(),
            Some(&plain[..])
        );
    }

    // --- segmentation fixture helpers --------------------------------------

    /// Builds a raw (non-PFB) Type1 program: ASCII `clear_ascii`, the
    /// `eexec` token, then `eexec_plain` encrypted with the eexec key/skip.
    fn raw_program(clear_ascii: &str, eexec_plain: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(clear_ascii.as_bytes());
        p.extend_from_slice(b"eexec\n");
        p.extend_from_slice(&encrypt(eexec_plain, EEXEC_KEY, EEXEC_SKIP));
        p
    }

    /// Builds one PFB segment: `0x80 <seg_type> <len:u32 LE> <data>`.
    fn pfb_segment(seg_type: u8, data: &[u8]) -> Vec<u8> {
        let mut s = vec![0x80, seg_type];
        s.extend_from_slice(&(data.len() as u32).to_le_bytes());
        s.extend_from_slice(data);
        s
    }

    // --- segmentation: raw, hex, PFB ----------------------------------------

    #[test]
    fn segment_raw_splits_clear_and_decrypts_eexec() {
        let prog = raw_program("%!FontType1\n/FontName /X def\n", b"/lenIV 4 def");
        let (clear, priv_dec) = segment(&prog).expect("segment");
        assert!(clear.starts_with(b"%!FontType1"));
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_hex_eexec_is_decoded_then_decrypted() {
        // Same content, but the eexec region is ASCII-hex instead of binary.
        let bin = encrypt(b"/lenIV 4 def", EEXEC_KEY, EEXEC_SKIP);
        let mut hex = String::new();
        for b in &bin {
            hex.push_str(&format!("{b:02x}"));
        }
        let mut prog = Vec::new();
        prog.extend_from_slice(b"%!\neexec\n");
        prog.extend_from_slice(hex.as_bytes());
        let (_clear, priv_dec) = segment(&prog).expect("segment");
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_pfb_concatenates_and_decrypts() {
        let clear = b"%!FontType1\n";
        let bin = encrypt(b"/lenIV 4 def", EEXEC_KEY, EEXEC_SKIP);
        let mut prog = pfb_segment(1, clear);
        prog.extend_from_slice(&pfb_segment(2, &bin));
        prog.extend_from_slice(&pfb_segment(3, b""));
        let (clear_out, priv_dec) = segment(&prog).expect("segment");
        assert!(clear_out.starts_with(b"%!FontType1"));
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_without_eexec_returns_none() {
        assert!(segment(b"%!FontType1\nno private here\n").is_none());
    }
}
