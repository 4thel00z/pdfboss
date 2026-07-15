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
//! This module decrypts and segments a program into its clear-text header
//! and decrypted private portion, then parses those bytes into a
//! [`Type1Font`]: `/FontMatrix`, `/Encoding`, `/Subrs`, and `/CharStrings`
//! (spec ch. 6 "Font Dictionary" and ch. 8 "Private Dictionary"; ISO 32000
//! §9.6.6.2). Interpreting each charstring into an outline is a later
//! task's job.

use std::collections::HashMap;

// --- Type1 stream cipher constants (spec ch. 7) -----------------------------

/// The fixed key for decrypting a `FontFile`'s `eexec` portion.
const EEXEC_KEY: u16 = 55665;
/// The fixed key for decrypting an individual charstring, once extracted
/// from the decrypted `eexec` portion (consumed by a later task).
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

/// PostScript whitespace (spec ch. 2; ISO 32000 Table 1): NUL, tab, LF, FF,
/// CR, space.
fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
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
/// `clear_text` is everything up to and including `eexec` and the single
/// whitespace separator immediately following it (a `\r\n` pair counts as
/// one separator, per spec ch. 7); the remainder is the (possibly
/// hex-encoded) `eexec` region, which is hex-decoded first when
/// [`looks_like_hex`] says so, then decrypted. `None` if the `eexec` token is
/// not present anywhere in `program`, or if decryption fails.
fn segment_raw(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    const TOKEN: &[u8] = b"eexec";
    let token_at = program.windows(TOKEN.len()).position(|w| w == TOKEN)?;
    let after_token = token_at + TOKEN.len();
    // Consume exactly ONE whitespace separator here, not a run: the eexec
    // ciphertext is arbitrary binary, so a real font's first ciphertext byte
    // may itself equal a whitespace value, and eating more than the one
    // separator the spec defines would misalign the whole decrypt window.
    let region_start = if program.get(after_token) == Some(&b'\r')
        && program.get(after_token + 1) == Some(&b'\n')
    {
        after_token + 2 // CRLF is a single line terminator
    } else if program
        .get(after_token)
        .is_some_and(|&b| is_ps_whitespace(b))
    {
        after_token + 1
    } else {
        after_token
    };
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

// --- Type1Font: parsed program (spec ch. 6, 8) ------------------------------

/// Default `/lenIV` (spec ch. 8) when the decrypted private portion doesn't
/// declare one: the number of scrambled lead bytes each charstring/subr's
/// own (`CHARSTRING_KEY`-keyed) encryption drops, distinct from the fixed
/// `EEXEC_SKIP` the outer `eexec` layer always uses.
const DEFAULT_LEN_IV: usize = 4;

/// Cap on the number of `/CharStrings` entries accepted: also the largest
/// count a `u16` gid (`Type1Font::name_to_gid`, `gid_for_name`) can address,
/// so it doubles as the defensive bound a hostile declared `/CharStrings
/// <count>` calls for (this parser never allocates proportionally to that
/// declared count in the first place -- see `parse_charstrings` -- but the
/// accepted-entry cap still bounds the work a pathological input can cause).
const MAX_GLYPHS: usize = 65_536;

/// Cap on an individual `/Subrs` index (`dup <index> ...`): real fonts use
/// small, roughly contiguous indices, so an index at or beyond this is
/// almost certainly hostile input (e.g. `dup 4000000000 ...`) rather than
/// something worth growing `subrs` to match.
const MAX_SUBR_INDEX: usize = 65_536;

/// A parsed Type 1 font: the pieces needed to map a glyph name to a
/// charstring and (for a later task) interpret that charstring into an
/// outline.
pub(crate) struct Type1Font {
    /// Decrypted charstring bytes per glyph, indexed by gid (gid 0 ==
    /// ".notdef" when the font defines it; otherwise CharStrings appearance
    /// order).
    charstrings: Vec<Vec<u8>>,
    /// gid -> glyph name, parallel to `charstrings`.
    names: Vec<String>,
    /// name -> gid.
    name_to_gid: HashMap<String, u16>,
    /// Decrypted local subroutines, indexed by subr number (gaps -> empty).
    subrs: Vec<Vec<u8>>,
    /// The font's built-in `/Encoding`: code -> glyph name (256 slots).
    builtin_encoding: Box<[Option<String>; 256]>,
    units_per_em: f32,
}

impl Type1Font {
    /// Parses a decrypted-and-segmented Type1 program: [`segment`] splits it
    /// into a clear-text header and eexec-decrypted private portion, then
    /// `/FontMatrix` and `/Encoding` are read from the header and `/lenIV`,
    /// `/Subrs`, and `/CharStrings` from the private portion.
    ///
    /// `/Encoding`'s `StandardEncoding` token form is deliberately NOT
    /// expanded into a code -> name table here: this font's caller (a later
    /// task) resolves the PDF `/Encoding` entry first and only falls back to
    /// this font's built-in encoding when the PDF gives nothing, so the
    /// built-in `StandardEncoding` case is already covered from the PDF
    /// side. Only an explicit `dup <code> /<name> put` encoding array
    /// populates `builtin_encoding` here; the bare `StandardEncoding` token
    /// leaves every slot `None`.
    ///
    /// Returns `None` if `segment` fails, or if the program yields zero
    /// charstrings (nothing paintable).
    pub(crate) fn parse(program: Vec<u8>) -> Option<Type1Font> {
        let (clear, private) = segment(&program)?;

        let units_per_em = units_per_em_from_clear(&clear);
        let builtin_encoding = parse_encoding(&clear);

        let len_iv = parse_len_iv(&private);
        let subrs = parse_subrs(&private, len_iv);
        let (charstrings, names, name_to_gid) = parse_charstrings(&private, len_iv);

        if charstrings.is_empty() {
            return None;
        }

        Some(Type1Font {
            charstrings,
            names,
            name_to_gid,
            subrs,
            builtin_encoding,
            units_per_em,
        })
    }

    /// Number of glyphs (the CharStrings entries found).
    pub(crate) fn num_glyphs(&self) -> usize {
        self.charstrings.len()
    }

    /// Maps a glyph name to a glyph index.
    pub(crate) fn gid_for_name(&self, name: &str) -> Option<u16> {
        self.name_to_gid.get(name).copied()
    }

    /// The font's built-in `/Encoding` name for `code` (see `parse`'s doc
    /// comment for why the `StandardEncoding` form leaves this `None`
    /// throughout).
    pub(crate) fn builtin_name(&self, code: u8) -> Option<&str> {
        self.builtin_encoding[code as usize].as_deref()
    }

    /// Font design units per em, from `/FontMatrix` (default 1000; see
    /// `units_per_em_from_clear`).
    pub(crate) fn units_per_em(&self) -> f32 {
        self.units_per_em
    }
}

// --- private-text/clear-text tokenizing (bounds-checked throughout) --------

/// Finds the first occurrence of `needle` in `haystack`, or `None`. (A
/// `needle` longer than `haystack` simply yields no windows, not a panic.)
fn find_token(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Reads the next whitespace-delimited token at or after `i` (leading
/// whitespace is skipped first). Returns the token and the index
/// immediately following it -- which, per the grammar this module parses, is
/// where the single mandatory separator byte before a binary blob lives.
/// `None` once no token remains. Every read goes through `.get()`, so a
/// wildly out-of-range `i` just yields `None` rather than panicking.
fn next_token(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let mut p = i;
    while bytes.get(p).is_some_and(|&b| is_ps_whitespace(b)) {
        p += 1;
    }
    let start = p;
    while bytes.get(p).is_some_and(|&b| !is_ps_whitespace(b)) {
        p += 1;
    }
    if p == start {
        return None;
    }
    bytes.get(start..p).map(|tok| (tok, p))
}

/// Parses a token as a non-negative decimal integer.
fn parse_uint_token(tok: &[u8]) -> Option<usize> {
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// Parses a token as an `f64`, tolerating a leading `[` (the matrix's first
/// value is conventionally fused with its opening bracket, e.g. `[0.001`).
fn parse_matrix_number(tok: &[u8]) -> Option<f64> {
    let tok = tok.strip_prefix(b"[").unwrap_or(tok);
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// Attempts to read one `<len> RD <len bytes>` (or `-|`) binary object (spec
/// ch. 6) starting the token scan at `i`: the next token must be a decimal
/// length, the one after it the binary-read marker (`RD` or `-|`), and then
/// exactly the single separator byte the spec requires before the binary
/// data itself begins. Returns the still-`CHARSTRING_KEY`-encrypted blob
/// (truncated, not panicking, if `len` runs past the end of `bytes`) and the
/// index immediately following it. `None` if the pattern doesn't match at
/// `i` at all (no decimal length there, or no recognized marker after it) --
/// the caller is responsible for advancing the scan itself in that case.
fn read_rd_blob(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let (len_tok, after_len) = next_token(bytes, i)?;
    let len = parse_uint_token(len_tok)?;
    let (marker, after_marker) = next_token(bytes, after_len)?;
    if marker != b"RD" && marker != b"-|" {
        return None;
    }
    if !bytes
        .get(after_marker)
        .is_some_and(|&b| is_ps_whitespace(b))
    {
        return None; // exactly one separator byte must follow the marker
    }
    let blob_start = after_marker.checked_add(1)?;
    let blob_end = blob_start.saturating_add(len).min(bytes.len());
    let blob = bytes.get(blob_start..blob_end)?;
    Some((blob, blob_end))
}

/// Reads `/lenIV <int> def` from the decrypted private portion (spec ch. 8),
/// defaulting to [`DEFAULT_LEN_IV`] if absent or unparsable.
fn parse_len_iv(private: &[u8]) -> usize {
    let Some(pos) = find_token(private, b"/lenIV") else {
        return DEFAULT_LEN_IV;
    };
    let after = pos.saturating_add(b"/lenIV".len());
    next_token(private, after)
        .and_then(|(tok, _)| parse_uint_token(tok))
        .unwrap_or(DEFAULT_LEN_IV)
}

/// Parses the decrypted private portion's `/Subrs <count> array` block (spec
/// ch. 8): repeated `dup <index> <len> RD <len bytes> NP` entries (the
/// terminator -- `NP`/`|`/`noaccess put` -- is never itself inspected; the
/// pattern is keyed off `<len> RD` alone, per this module's leniency
/// convention). Each blob is decrypted with `decrypt(_, CHARSTRING_KEY,
/// len_iv)`. Indexed into the result by `<index>` (gaps become empty
/// `Vec`s); an index `>= MAX_SUBR_INDEX`, or a blob that fails to decrypt, is
/// skipped rather than acted on. The scan stops at `/CharStrings` (Subrs
/// entries never appear past it) or the end of `private`, whichever comes
/// first -- this also keeps a spurious `dup` inside `/CharStrings` (there
/// shouldn't be one, but this parser is deliberately lenient) from being
/// mistaken for a Subrs entry.
fn parse_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let mut subrs: Vec<Vec<u8>> = Vec::new();
    let Some(subrs_pos) = find_token(private, b"/Subrs") else {
        return subrs;
    };
    let tail = private.get(subrs_pos..).unwrap_or(&[]);
    let scan_end = find_token(tail, b"/CharStrings")
        .map(|off| subrs_pos.saturating_add(off))
        .unwrap_or(private.len());

    let mut i = subrs_pos;
    while i < scan_end {
        let Some((tok, after_tok)) = next_token(private, i) else {
            break;
        };
        if tok != b"dup" {
            i = after_tok;
            continue;
        }
        let Some((idx_tok, after_idx)) = next_token(private, after_tok) else {
            i = after_tok;
            continue;
        };
        let Some(index) = parse_uint_token(idx_tok) else {
            i = after_idx;
            continue;
        };
        let Some((blob, end)) = read_rd_blob(private, after_idx) else {
            i = after_idx;
            continue;
        };
        if index < MAX_SUBR_INDEX {
            if index >= subrs.len() {
                subrs.resize(index + 1, Vec::new());
            }
            if let Some(decoded) = decrypt(blob, CHARSTRING_KEY, len_iv) {
                subrs[index] = decoded;
            }
        }
        i = end;
    }
    subrs
}

/// Parses the decrypted private portion's `/CharStrings <count> dict dup
/// begin` block (spec ch. 8): repeated `/<name> <len> RD <len bytes> ND`
/// entries (terminator -- `ND`/`|-`/`noaccess def` -- not itself inspected,
/// same leniency convention as [`parse_subrs`]). gid is assignment order: a
/// literal `.notdef` entry keeps its natural order rather than being forced
/// to gid 0 (the loader treats gid 0 as "not found"; a real `.notdef`
/// charstring landing elsewhere is harmless). Each blob is decrypted with
/// `decrypt(_, CHARSTRING_KEY, len_iv)`; a blob that fails to decrypt is
/// skipped. Accepts at most [`MAX_GLYPHS`] entries.
fn parse_charstrings(
    private: &[u8],
    len_iv: usize,
) -> (Vec<Vec<u8>>, Vec<String>, HashMap<String, u16>) {
    let mut charstrings: Vec<Vec<u8>> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut name_to_gid: HashMap<String, u16> = HashMap::new();

    let Some(cs_pos) = find_token(private, b"/CharStrings") else {
        return (charstrings, names, name_to_gid);
    };

    let mut i = cs_pos;
    while i < private.len() {
        if charstrings.len() >= MAX_GLYPHS {
            break;
        }
        let Some((tok, after_tok)) = next_token(private, i) else {
            break;
        };
        let Some(name_bytes) = tok.strip_prefix(b"/") else {
            i = after_tok;
            continue;
        };
        let Some((blob, end)) = read_rd_blob(private, after_tok) else {
            i = after_tok;
            continue;
        };
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            i = end;
            continue;
        };
        let Some(decoded) = decrypt(blob, CHARSTRING_KEY, len_iv) else {
            i = end;
            continue;
        };
        let gid = charstrings.len() as u16; // charstrings.len() < MAX_GLYPHS <= u16::MAX + 1
        charstrings.push(decoded);
        names.push(name.to_string());
        name_to_gid.insert(name.to_string(), gid);
        i = end;
    }
    (charstrings, names, name_to_gid)
}

/// Parses the clear-text header's `/Encoding` declaration (spec ch. 6): a
/// custom encoding array's `dup <code> /<name> put` entries populate
/// `builtin_encoding[code]`. The other legal form -- the bare token
/// `StandardEncoding` -- has no such entries to find, so it (and any font
/// with no `/Encoding` at all) simply yields every slot `None`; see
/// `Type1Font::parse`'s doc comment for why that is an acceptable v1
/// simplification.
fn parse_encoding(clear: &[u8]) -> Box<[Option<String>; 256]> {
    let mut table: Box<[Option<String>; 256]> = Box::new(std::array::from_fn(|_| None));
    let Some(enc_pos) = find_token(clear, b"/Encoding") else {
        return table;
    };

    let mut i = enc_pos;
    while i < clear.len() {
        let Some((tok, after_tok)) = next_token(clear, i) else {
            break;
        };
        if tok != b"dup" {
            i = after_tok;
            continue;
        }
        let Some((code_tok, after_code)) = next_token(clear, after_tok) else {
            i = after_tok;
            continue;
        };
        let Some(code) = parse_uint_token(code_tok) else {
            i = after_code;
            continue;
        };
        let Some((name_tok, after_name)) = next_token(clear, after_code) else {
            i = after_code;
            continue;
        };
        let Some(name_bytes) = name_tok.strip_prefix(b"/") else {
            i = after_code;
            continue;
        };
        let Some((put_tok, after_put)) = next_token(clear, after_name) else {
            i = after_name;
            continue;
        };
        if put_tok != b"put" {
            i = after_name;
            continue;
        }
        if let Ok(name) = std::str::from_utf8(name_bytes) {
            if let Some(slot) = table.get_mut(code) {
                *slot = Some(name.to_string());
            }
        }
        i = after_put;
    }
    table
}

/// Computes units-per-em from the clear-text header's `/FontMatrix [a b c d
/// e f]` (spec ch. 6): `(1.0 / a).abs()`, or 1000.0 if `/FontMatrix` is
/// absent, unparsable, or `a` is zero. Mirrors the convention
/// `cff.rs::units_per_em_from_top_dict` uses for the CFF Top DICT's
/// `FontMatrix`.
fn units_per_em_from_clear(clear: &[u8]) -> f32 {
    let Some(pos) = find_token(clear, b"/FontMatrix") else {
        return 1000.0;
    };
    let after = pos.saturating_add(b"/FontMatrix".len());
    let a = next_token(clear, after).and_then(|(tok, _)| parse_matrix_number(tok));
    match a {
        Some(a) if a != 0.0 => (1.0_f64 / a).abs() as f32,
        _ => 1000.0,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The shared cipher's inverse: same recurrence, but `C` is the byte this
    /// function itself just emitted (rather than one read from ciphertext).
    /// Prepends `skip` zero-valued filler bytes before encrypting, mirroring
    /// what `decrypt` then drops.
    fn encrypt(plain: &[u8], key: u16, skip: usize) -> Vec<u8> {
        encrypt_with_lead(plain, key, &vec![0u8; skip])
    }

    /// Like [`encrypt`], but the caller supplies the lead filler bytes
    /// explicitly instead of always zero-filling them. Lets a test pin the
    /// resulting first *ciphertext* byte to a specific value (by choosing
    /// the corresponding lead plaintext byte), since the cipher's first
    /// emitted byte depends only on `key` and `lead[0]`.
    fn encrypt_with_lead(plain: &[u8], key: u16, lead: &[u8]) -> Vec<u8> {
        let mut r = key;
        let mut out = Vec::new();
        let mut buf = lead.to_vec();
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
    fn segment_raw_single_separator_does_not_eat_a_whitespace_valued_ciphertext_byte() {
        // Force the eexec region's first *ciphertext* byte (C0) to be 10
        // (0x0A, a newline value) to prove the fix consumes exactly one
        // separator after `eexec`, not a run: C0 = leadByte ^ high_byte(EEXEC_KEY)
        // = leadByte ^ 217 (EEXEC_KEY = 55665 = 0xD971, high byte 0xD9 = 217).
        // Solving leadByte ^ 217 == 10 gives leadByte == 211 (211 ^ 217 == 10).
        // `lead` supplies all EEXEC_SKIP (=4) filler bytes; only byte 0 feeds
        // C0, so the rest stay 0.
        let lead = [211u8, 0, 0, 0];
        let plain = b"/lenIV 4 def";
        let cipher = encrypt_with_lead(plain, EEXEC_KEY, &lead);
        assert_eq!(
            cipher[0], 10,
            "test setup: first ciphertext byte must itself be a whitespace (newline) value"
        );

        let mut program = Vec::new();
        program.extend_from_slice(b"%!FontType1\neexec\n");
        program.extend_from_slice(&cipher);

        // The old (buggy) run-skipping logic would treat this leading 0x0A
        // ciphertext byte as more separator whitespace and consume it too,
        // shifting the whole decrypt window by one byte and corrupting the
        // result. The fixed logic stops after the single literal `\n` that
        // follows `eexec` in the program text, leaving `cipher` untouched.
        let (_clear, priv_dec) = segment(&program).expect("segment");
        assert_eq!(&priv_dec, plain);
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

    // --- adversarial-input leniency: never panic, always None on garbage ---

    #[test]
    fn segment_pfb_truncated_length_field_returns_none() {
        // A type-2 (binary) segment marker (128 = 0x80, 2) followed by only
        // one byte (16) of what should be a 4-byte little-endian length --
        // the header is truncated before the length field is complete.
        let program = [128u8, 2, 16];
        assert!(segment(&program).is_none());
    }

    #[test]
    fn segment_pfb_length_exceeds_available_bytes_returns_none() {
        // A well-formed type-2 header declaring a 100-byte payload (little-
        // endian 100, 0, 0, 0), but the program ends right after the header
        // with zero bytes of actual data present.
        let program = [128u8, 2, 100, 0, 0, 0];
        assert!(segment(&program).is_none());
    }

    #[test]
    fn segment_empty_input_returns_none() {
        assert!(segment(&[]).is_none());
    }

    // --- Type1Font::parse fixture helpers -----------------------------------
    //
    // Charstring bytes are built from small decimal command encoders (spec
    // ch. 6.2's Type1 charstring number encoding) rather than literal hex/
    // binary blobs, per this codebase's clean-room fixture convention.

    /// Encodes one Type1 charstring number operand (spec ch. 6.2), decimal
    /// only.
    fn cs_num(out: &mut Vec<u8>, v: i32) {
        if (-107..=107).contains(&v) {
            out.push((v + 139) as u8);
        } else if (108..=1131).contains(&v) {
            let v = v - 108;
            out.push((v / 256 + 247) as u8);
            out.push((v % 256) as u8);
        } else if (-1131..=-108).contains(&v) {
            let v = -v - 108;
            out.push((v / 256 + 251) as u8);
            out.push((v % 256) as u8);
        } else {
            out.push(255);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }

    /// Encodes a one-byte Type1 charstring operator (1..31).
    fn cs_op(out: &mut Vec<u8>, op: u8) {
        out.push(op);
    }

    /// Encodes an escape (`12 x`) Type1 charstring operator.
    fn cs_escape(out: &mut Vec<u8>, op: u8) {
        out.push(12);
        out.push(op);
    }

    /// A minimal glyph: `hsbw(0,1000)` then `endchar`.
    fn stub_charstring() -> Vec<u8> {
        let mut c = Vec::new();
        cs_num(&mut c, 0);
        cs_num(&mut c, 1000);
        cs_op(&mut c, 13); // hsbw
        cs_op(&mut c, 14); // endchar
        c
    }

    /// Builds a raw (non-PFB) Type1 program with a clear-text header
    /// (`/FontMatrix`, `/Encoding`) and an eexec-encrypted private portion
    /// (`/lenIV`, `/Subrs`, `/CharStrings`), mirroring the grammar
    /// `Type1Font::parse` reads. `subrs` is `&[(index, plaintext
    /// charstring)]` (index need not be contiguous or sorted); `charstrings`
    /// is `&[(name, plaintext charstring)]`, emitted -- and so assigned gids
    /// -- in the given order. Each charstring/subr blob is independently
    /// encrypted with the charstring key/`len_iv` before being embedded in
    /// the (separately eexec-encrypted) private text, exactly as a real
    /// font nests the two ciphers.
    fn build_type1_program(
        font_matrix: &str,
        encoding: &[(u8, &str)],
        charstrings: &[(&str, Vec<u8>)],
        subrs: &[(u16, Vec<u8>)],
        len_iv: usize,
    ) -> Vec<u8> {
        let mut clear = String::new();
        clear.push_str("%!\n");
        clear.push_str(&format!("/FontMatrix {font_matrix} def\n"));
        clear.push_str("/Encoding 256 array\n");
        for (code, name) in encoding {
            clear.push_str(&format!("dup {code} /{name} put\n"));
        }

        let mut private = Vec::new();
        private.extend_from_slice(format!("/lenIV {len_iv} def\n").as_bytes());
        private.extend_from_slice(format!("/Subrs {} array\n", subrs.len()).as_bytes());
        for (index, plain) in subrs {
            let blob = encrypt(plain, CHARSTRING_KEY, len_iv);
            private.extend_from_slice(format!("dup {index} {} RD ", blob.len()).as_bytes());
            private.extend_from_slice(&blob);
            private.extend_from_slice(b" NP\n");
        }
        private.extend_from_slice(
            format!("/CharStrings {} dict dup begin\n", charstrings.len()).as_bytes(),
        );
        for (name, plain) in charstrings {
            let blob = encrypt(plain, CHARSTRING_KEY, len_iv);
            private.extend_from_slice(format!("/{name} {} RD ", blob.len()).as_bytes());
            private.extend_from_slice(&blob);
            private.extend_from_slice(b" ND\n");
        }
        private.extend_from_slice(b"end");

        raw_program(&clear, &private)
    }

    // --- Type1Font::parse ----------------------------------------------------

    #[test]
    fn parse_reads_charstrings_encoding_and_matrix() {
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[(65u8, "A"), (66, "B")],
            &[
                (".notdef", stub_charstring()),
                ("A", stub_charstring()),
                ("B", stub_charstring()),
            ],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        assert_eq!(f.num_glyphs(), 3);
        assert!(f.gid_for_name("A").is_some());
        assert!(f.gid_for_name("B").is_some());
        assert!(f.gid_for_name("nonesuch").is_none());
        assert_eq!(f.builtin_name(65), Some("A"));
        assert_eq!(f.builtin_name(66), Some("B"));
        assert_eq!(f.units_per_em(), 1000.0);
    }

    #[test]
    fn parse_reads_non_default_font_matrix() {
        let prog = build_type1_program(
            "[0.0005 0 0 0.0005 0 0]",
            &[],
            &[(".notdef", stub_charstring())],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        assert_eq!(f.units_per_em(), 2000.0);
    }

    #[test]
    fn parse_rejects_program_without_charstrings() {
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[], &[], 4);
        assert!(Type1Font::parse(prog).is_none()); // no glyphs -> not paintable
    }

    #[test]
    fn parse_tolerates_truncated_charstring_blob() {
        // Declare a length longer than the bytes actually present; parse must
        // not panic.
        let mut prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("A", stub_charstring())],
            &[],
            4,
        );
        prog.truncate(prog.len() - 3); // chop the tail
        let _ = Type1Font::parse(prog); // must return Some or None, never panic
    }
}
