//! ToUnicode CMap parsing: `begincodespacerange`, `beginbfchar`, and both
//! `beginbfrange` forms; destination hex is UTF-16BE and may be multi-char.

use pdfboss_core::lexer::{Lexer, Token};
use std::collections::HashMap;

/// A parsed ToUnicode CMap mapping character codes to Unicode strings.
///
/// Parsing is lenient: unrecognized tokens are skipped and malformed
/// sections contribute nothing, so [`ToUnicode::parse`] never fails.
#[derive(Debug, Default)]
pub struct ToUnicode {
    /// `(byte_len, low, high)` from `begincodespacerange`.
    codespaces: Vec<(usize, u32, u32)>,
    /// Single-code mappings (`bfchar` and array-form `bfrange`).
    singles: HashMap<u32, String>,
    /// Increment-form `bfrange` entries: `(low, high, base UTF-16 units)`;
    /// the last unit increments with the code.
    ranges: Vec<(u32, u32, Vec<u16>)>,
}

/// Folds up to the last 4 bytes of a hex-string source code, big-endian.
fn code_value(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
}

/// Splits destination hex bytes into UTF-16BE code units; a trailing odd
/// byte becomes its own unit (lenient).
fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|c| {
            if c.len() == 2 {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from(c[0])
            }
        })
        .collect()
}

impl ToUnicode {
    /// Parses a decoded ToUnicode CMap stream. Never fails; anything the
    /// parser does not understand is skipped.
    pub fn parse(data: &[u8]) -> ToUnicode {
        let mut out = ToUnicode::default();
        let mut lx = Lexer::new(data);
        loop {
            match next_or_skip(&mut lx, data.len()) {
                None => break,
                Some(Token::Keyword(kw)) => match kw.as_slice() {
                    b"begincodespacerange" => out.parse_codespaces(&mut lx, data.len()),
                    b"beginbfchar" => out.parse_bfchars(&mut lx, data.len()),
                    b"beginbfrange" => out.parse_bfranges(&mut lx, data.len()),
                    _ => {}
                },
                Some(_) => {}
            }
        }
        out
    }

    /// Looks up the Unicode string for `code`, if mapped.
    pub fn lookup(&self, code: u32) -> Option<String> {
        if let Some(s) = self.singles.get(&code) {
            return Some(s.clone());
        }
        for &(lo, hi, ref base) in &self.ranges {
            if (lo..=hi).contains(&code) {
                let mut units = base.clone();
                if let Some(last) = units.last_mut() {
                    *last = last.wrapping_add((code - lo) as u16);
                }
                return Some(String::from_utf16_lossy(&units));
            }
        }
        None
    }

    /// Number of bytes in the next code starting at `bytes`, per the
    /// codespace ranges (shortest matching range wins). `None` when no
    /// range matches. (Code splitting for extraction is fixed at 1 byte
    /// for simple fonts and 2 for Type0, so this is diagnostic-only.)
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn code_len(&self, bytes: &[u8]) -> Option<usize> {
        let mut lens: Vec<usize> = self.codespaces.iter().map(|&(n, _, _)| n).collect();
        lens.sort_unstable();
        lens.dedup();
        for n in lens {
            if bytes.len() < n {
                continue;
            }
            let v = code_value(&bytes[..n]);
            for &(cn, lo, hi) in &self.codespaces {
                if cn == n && (lo..=hi).contains(&v) {
                    return Some(n);
                }
            }
        }
        None
    }

    /// True when no mappings at all were found.
    pub fn is_empty(&self) -> bool {
        self.singles.is_empty() && self.ranges.is_empty()
    }

    /// Reads `<lo> <hi>` pairs until `endcodespacerange`.
    fn parse_codespaces(&mut self, lx: &mut Lexer<'_>, len: usize) {
        loop {
            let lo = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => h,
                Some(_) | None => return, // `endcodespacerange` or junk
            };
            let Some(Token::HexString(hi)) = next_or_skip(lx, len) else {
                return;
            };
            if lo.is_empty() {
                continue;
            }
            self.codespaces
                .push((lo.len(), code_value(&lo), code_value(&hi)));
        }
    }

    /// Reads `<src> <dst>` pairs until `endbfchar`.
    fn parse_bfchars(&mut self, lx: &mut Lexer<'_>, len: usize) {
        loop {
            let src = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => h,
                Some(_) | None => return,
            };
            match next_or_skip(lx, len) {
                Some(Token::HexString(dst)) => {
                    let units = utf16_units(&dst);
                    if !units.is_empty() {
                        self.singles
                            .insert(code_value(&src), String::from_utf16_lossy(&units));
                    }
                }
                // A name destination (base-font form) or junk: skip entry.
                Some(_) => {}
                None => return,
            }
        }
    }

    /// Reads `<lo> <hi> (<dst> | [<dst>…])` triples until `endbfrange`.
    fn parse_bfranges(&mut self, lx: &mut Lexer<'_>, len: usize) {
        loop {
            let lo = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => code_value(&h),
                Some(_) | None => return,
            };
            let hi = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => code_value(&h),
                Some(_) | None => return,
            };
            match next_or_skip(lx, len) {
                Some(Token::HexString(dst)) => {
                    let units = utf16_units(&dst);
                    if !units.is_empty() && lo <= hi {
                        self.ranges.push((lo, hi, units));
                    }
                }
                Some(Token::ArrayOpen) => {
                    let mut code = lo;
                    loop {
                        match next_or_skip(lx, len) {
                            Some(Token::HexString(dst)) => {
                                let units = utf16_units(&dst);
                                if !units.is_empty() && code <= hi {
                                    self.singles.insert(code, String::from_utf16_lossy(&units));
                                }
                                code = code.saturating_add(1);
                            }
                            Some(Token::ArrayClose) => break,
                            Some(_) => {}
                            None => return,
                        }
                    }
                }
                Some(_) => {}
                None => return,
            }
        }
    }
}

/// Fetches the next token, force-advancing past unlexable bytes; `None`
/// at end of input.
fn next_or_skip(lx: &mut Lexer<'_>, len: usize) -> Option<Token> {
    loop {
        let before = lx.pos();
        match lx.next_token() {
            Ok(Token::Eof) => return None,
            Ok(t) => return Some(t),
            Err(_) => {
                if lx.pos() <= before {
                    if before + 1 >= len {
                        return None;
                    }
                    lx.seek(before + 1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bfchar_single_and_multi_char() {
        let cmap = ToUnicode::parse(
            b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
              2 beginbfchar\n<41> <0042>\n<01> <00660066>\nendbfchar\n\
              endcmap end end",
        );
        assert_eq!(cmap.lookup(0x41).as_deref(), Some("B"));
        assert_eq!(cmap.lookup(0x01).as_deref(), Some("ff"));
        assert_eq!(cmap.lookup(0x42), None);
    }

    #[test]
    fn bfrange_increment_crosses_byte_boundary() {
        let cmap = ToUnicode::parse(b"1 beginbfrange <20> <22> <00FE> endbfrange");
        assert_eq!(cmap.lookup(0x20).as_deref(), Some("\u{FE}"));
        assert_eq!(cmap.lookup(0x21).as_deref(), Some("\u{FF}"));
        assert_eq!(cmap.lookup(0x22).as_deref(), Some("\u{100}"));
        assert_eq!(cmap.lookup(0x23), None);
        assert_eq!(cmap.lookup(0x1F), None);
    }

    #[test]
    fn bfrange_array_form() {
        let cmap =
            ToUnicode::parse(b"1 beginbfrange <41> <43> [<0058> <0059005A> <005A>] endbfrange");
        assert_eq!(cmap.lookup(0x41).as_deref(), Some("X"));
        assert_eq!(cmap.lookup(0x42).as_deref(), Some("YZ"));
        assert_eq!(cmap.lookup(0x43).as_deref(), Some("Z"));
        assert_eq!(cmap.lookup(0x44), None);
    }

    #[test]
    fn bfrange_multi_unit_increments_last() {
        let cmap = ToUnicode::parse(b"1 beginbfrange <00> <01> <00410030> endbfrange");
        assert_eq!(cmap.lookup(0x00).as_deref(), Some("A0"));
        assert_eq!(cmap.lookup(0x01).as_deref(), Some("A1"));
    }

    #[test]
    fn surrogate_pair_destination() {
        let cmap = ToUnicode::parse(b"1 beginbfchar <05> <D83DDE00> endbfchar");
        assert_eq!(cmap.lookup(0x05).as_deref(), Some("\u{1F600}"));
    }

    #[test]
    fn two_byte_codespace_gives_code_len() {
        let cmap = ToUnicode::parse(
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
              1 beginbfchar <2126> <03A9> endbfchar",
        );
        assert_eq!(cmap.code_len(&[0x21, 0x26]), Some(2));
        assert_eq!(cmap.code_len(&[0x21]), None); // not enough bytes
        assert_eq!(cmap.lookup(0x2126).as_deref(), Some("\u{3A9}"));
    }

    #[test]
    fn mixed_codespace_widths() {
        let cmap =
            ToUnicode::parse(b"2 begincodespacerange <00> <7F> <8000> <FFFF> endcodespacerange");
        assert_eq!(cmap.code_len(&[0x41, 0x00]), Some(1));
        assert_eq!(cmap.code_len(&[0x80, 0x01]), Some(2));
        assert_eq!(cmap.code_len(&[0xFF]), None);
        assert!(cmap.is_empty());
    }

    #[test]
    fn garbage_is_ignored() {
        let cmap = ToUnicode::parse(b"\xFF\xFE junk ) ] >> beginbfchar <01> <0041> endbfchar");
        assert_eq!(cmap.lookup(0x01).as_deref(), Some("A"));
        let empty = ToUnicode::parse(b"");
        assert!(empty.is_empty());
        assert_eq!(empty.code_len(&[0x00]), None);
    }
}
