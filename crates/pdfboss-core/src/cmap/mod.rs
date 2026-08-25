//! CID CMap parsing and code splitting for composite (Type0) fonts
//! (ISO 32000-1 §9.7.5-9.7.6): `begincodespacerange` drives variable-width
//! code splitting, `begincidrange`/`begincidchar` map codes to CIDs,
//! `usecmap` layers a CMap over another, and `/WMode` selects the writing
//! mode. The value domain is codes to CID integers — distinct from the
//! ToUnicode CMaps parsed in `pdfboss-text`, whose destinations are text.

mod predefined;

pub use predefined::{cid_to_unicode, predefined, CidToUnicode};

use crate::document::decoded_stream_data_with;
use crate::hash::FastMap;
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Object, Stream};
use crate::source::AsyncObjectSource;
use std::sync::Arc;

/// One `begincodespacerange` entry: byte-wise lower and upper bounds for
/// codes of `len` bytes (ISO 32000-1 §9.7.6.2 — a code matches when every
/// byte lies within the bounds at its position, not when the folded value
/// does).
#[derive(Clone, Copy)]
struct Codespace {
    len: u8,
    lo: [u8; 4],
    hi: [u8; 4],
}

impl Codespace {
    fn contains(&self, code: &[u8]) -> bool {
        code.iter()
            .zip(self.lo.iter().zip(&self.hi))
            .all(|(&b, (&lo, &hi))| lo <= b && b <= hi)
    }
}

/// One `begincidrange` (or `beginnotdefrange`) entry: codes of `len` bytes
/// from `lo` to `hi` map to consecutive CIDs from `cid`.
#[derive(Clone, Copy)]
struct CidRange {
    len: u8,
    lo: u32,
    hi: u32,
    cid: u32,
}

/// A parsed CID CMap. Parsing is lenient: unrecognized tokens are skipped
/// and malformed sections contribute what they parsed so far, so
/// [`CidCmap::parse`] never fails.
pub struct CidCmap {
    wmode: u8,
    /// Own plus inherited codespaces, sorted by length so the shortest
    /// match wins at [`CidCmap::code_at`].
    codespaces: Vec<Codespace>,
    /// `begincidchar` singletons, keyed by (byte length, code value).
    singles: FastMap<(u8, u32), u32>,
    /// `begincidrange` entries, sorted by (length, low) for binary search.
    ranges: Vec<CidRange>,
    /// `beginnotdefrange`/`beginnotdefchar` entries, consulted only after
    /// every real mapping in the chain has missed.
    notdefs: Vec<CidRange>,
    /// The `usecmap` layer underneath: any mapping here loses to any
    /// mapping above, exactly the child-overrides-parent the operator asks.
    parent: Option<Arc<CidCmap>>,
}

/// Folds up to the last 4 bytes of a code, big-endian.
fn code_value(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
}

/// The range covering `code` within `ranges` (sorted by `(len, lo)`), if any.
fn covering_range(ranges: &[CidRange], code: u32, len: u8) -> Option<&CidRange> {
    let idx = ranges.partition_point(|r| (r.len, r.lo) <= (len, code));
    let r = ranges.get(idx.checked_sub(1)?)?;
    (r.len == len && code <= r.hi).then_some(r)
}

impl CidCmap {
    /// The Identity mapping (ISO 32000-1 Table 118, `Identity-H`/`-V`):
    /// 2-byte codes, CID == code.
    pub fn identity(vertical: bool) -> CidCmap {
        CidCmap {
            wmode: u8::from(vertical),
            codespaces: vec![Codespace {
                len: 2,
                lo: [0; 4],
                hi: [0xFF; 4],
            }],
            singles: FastMap::default(),
            ranges: vec![CidRange {
                len: 2,
                lo: 0,
                hi: 0xFFFF,
                cid: 0,
            }],
            notdefs: Vec::new(),
            parent: None,
        }
    }

    /// Parses a decoded CMap with no way to resolve `usecmap` names.
    pub fn parse(data: &[u8]) -> CidCmap {
        CidCmap::parse_with(data, None, &mut |_| None)
    }

    /// Parses a decoded CMap. `parent` is the layer named by the stream
    /// dictionary's `/UseCMap`, if any; an in-content `usecmap` operator
    /// resolves through `resolve` and fills the parent slot only when it is
    /// still empty (a CMap has one parent).
    pub fn parse_with(
        data: &[u8],
        parent: Option<Arc<CidCmap>>,
        resolve: &mut dyn FnMut(&str) -> Option<Arc<CidCmap>>,
    ) -> CidCmap {
        let mut out = CidCmap {
            wmode: 0,
            codespaces: Vec::new(),
            singles: FastMap::default(),
            ranges: Vec::new(),
            notdefs: Vec::new(),
            parent,
        };
        let mut lx = Lexer::new(data);
        let mut pending_name: Option<String> = None;
        let mut wmode_pending = false;
        loop {
            match next_or_skip(&mut lx, data.len()) {
                None => break,
                Some(Token::Keyword(kw)) => {
                    match kw.as_slice() {
                        b"begincodespacerange" => out.parse_codespaces(&mut lx, data.len()),
                        b"begincidchar" => out.parse_cidchars(&mut lx, data.len(), false),
                        b"begincidrange" => out.parse_cidranges(&mut lx, data.len(), false),
                        b"beginnotdefchar" => out.parse_cidchars(&mut lx, data.len(), true),
                        b"beginnotdefrange" => out.parse_cidranges(&mut lx, data.len(), true),
                        b"usecmap" => {
                            if let (None, Some(name)) = (&out.parent, pending_name.take()) {
                                out.parent = resolve(&name);
                            }
                        }
                        _ => {}
                    }
                    pending_name = None;
                    wmode_pending = false;
                }
                Some(Token::Name(n)) => {
                    wmode_pending = n.0 == "WMode";
                    pending_name = (!wmode_pending).then_some(n.0);
                }
                Some(Token::Int(i)) => {
                    if wmode_pending {
                        out.wmode = u8::from(i == 1);
                    }
                    wmode_pending = false;
                }
                Some(_) => {
                    pending_name = None;
                    wmode_pending = false;
                }
            }
        }
        out.finish();
        out
    }

    /// Sorts the lookup tables and folds the parent's codespaces in, so
    /// splitting sees the union while CID lookups stay layered.
    fn finish(&mut self) {
        if let Some(parent) = &self.parent {
            self.codespaces.extend_from_slice(&parent.codespaces);
        }
        self.codespaces.sort_by_key(|c| c.len);
        self.ranges.sort_by_key(|r| (r.len, r.lo));
        self.notdefs.sort_by_key(|r| (r.len, r.lo));
    }

    /// True when nothing at all was mapped — the caller's cue to treat an
    /// embedded CMap stream as unreadable rather than as "maps everything
    /// to CID 0".
    pub fn is_empty(&self) -> bool {
        self.singles.is_empty() && self.ranges.is_empty() && self.parent.is_none()
    }

    /// Writing mode: true for vertical (`/WMode 1`).
    pub fn vertical(&self) -> bool {
        self.wmode == 1
    }

    /// The `usecmap` layer underneath, if any. A vertical CMap's parent is
    /// its horizontal base, whose CIDs name the unrotated glyphs — the ones
    /// a CID-to-Unicode inversion actually knows.
    pub fn parent(&self) -> Option<&Arc<CidCmap>> {
        self.parent.as_ref()
    }

    /// True when the codespaces read single byte `b` as a complete code —
    /// the ISO 32000-1 §9.3.3 condition for word spacing to apply to a
    /// composite font's code 32.
    pub fn single_byte(&self, b: u8) -> bool {
        self.codespaces
            .iter()
            .any(|c| c.len == 1 && c.contains(&[b]))
    }

    /// Splits the next code from `bytes` at `pos` (which must be in
    /// bounds), returning `(value, length)`. The shortest codespace range
    /// that matches byte-wise wins; when none matches fully, the shortest
    /// range whose first byte fits still decides the length (the code maps
    /// to nothing and will read as notdef); when even that fails, one raw
    /// byte is consumed. Always consumes at least one byte. With no
    /// codespaces at all, codes are two bytes — the Type0 default this
    /// module's callers otherwise assume.
    pub fn code_at(&self, bytes: &[u8], pos: usize) -> (u32, u8) {
        let rest = &bytes[pos..];
        if self.codespaces.is_empty() {
            let n = rest.len().min(2);
            return (code_value(&rest[..n]), n as u8);
        }
        for cs in &self.codespaces {
            let n = usize::from(cs.len);
            if rest.len() >= n && cs.contains(&rest[..n]) {
                return (code_value(&rest[..n]), cs.len);
            }
        }
        for cs in &self.codespaces {
            if cs.lo[0] <= rest[0] && rest[0] <= cs.hi[0] {
                let n = usize::from(cs.len).min(rest.len());
                return (code_value(&rest[..n]), n as u8);
            }
        }
        (u32::from(rest[0]), 1)
    }

    /// The CID for a code of `len` bytes: this layer's `cidchar` singletons,
    /// then its `cidrange`s, then the parent chain, and only after every
    /// real mapping missed, the notdef entries. `None` reads as CID 0.
    pub fn cid(&self, code: u32, len: u8) -> Option<u32> {
        self.mapped(code, len).or_else(|| self.notdef(code, len))
    }

    fn mapped(&self, code: u32, len: u8) -> Option<u32> {
        if let Some(&cid) = self.singles.get(&(len, code)) {
            return Some(cid);
        }
        // Consecutive CIDs across the range (ISO 32000-1 §9.7.6.3).
        if let Some(r) = covering_range(&self.ranges, code, len) {
            return Some(r.cid.saturating_add(code - r.lo));
        }
        self.parent.as_ref()?.mapped(code, len)
    }

    // Every code of a notdef range maps to the one stated CID.
    fn notdef(&self, code: u32, len: u8) -> Option<u32> {
        covering_range(&self.notdefs, code, len)
            .map(|r| r.cid)
            .or_else(|| self.parent.as_ref()?.notdef(code, len))
    }

    /// Feeds every real mapping in the chain to `push` as
    /// `(len, lo, hi, first_cid)`, shallowest layer first and ascending by
    /// code within a layer — the iteration order that lets an inversion
    /// keep the lowest code for each CID.
    fn mappings(&self, push: &mut impl FnMut(u8, u32, u32, u32)) {
        let mut singles: Vec<(u8, u32, u32)> = self
            .singles
            .iter()
            .map(|(&(len, code), &cid)| (len, code, cid))
            .collect();
        singles.sort_unstable();
        let mut singles = singles.into_iter().peekable();
        let mut ranges = self.ranges.iter().peekable();
        loop {
            let single_first = match (singles.peek(), ranges.peek()) {
                (None, None) => break,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (Some(&(len, code, _)), Some(r)) => (len, code) <= (r.len, r.lo),
            };
            if single_first {
                let (len, code, cid) = singles.next().unwrap();
                push(len, code, code, cid);
            } else {
                let r = ranges.next().unwrap();
                push(r.len, r.lo, r.hi, r.cid);
            }
        }
        if let Some(parent) = &self.parent {
            parent.mappings(push);
        }
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
            if lo.is_empty() || lo.len() > 4 || hi.len() != lo.len() {
                continue;
            }
            let mut space = Codespace {
                len: lo.len() as u8,
                lo: [0; 4],
                hi: [0; 4],
            };
            space.lo[..lo.len()].copy_from_slice(&lo);
            space.hi[..hi.len()].copy_from_slice(&hi);
            self.codespaces.push(space);
        }
    }

    /// Reads `<code> cid` pairs until `endcidchar`/`endnotdefchar`.
    fn parse_cidchars(&mut self, lx: &mut Lexer<'_>, len: usize, notdef: bool) {
        loop {
            let code = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => h,
                Some(_) | None => return,
            };
            let Some(Token::Int(cid)) = next_or_skip(lx, len) else {
                return;
            };
            if code.is_empty() || code.len() > 4 {
                continue;
            }
            let (value, width) = (code_value(&code), code.len() as u8);
            let cid = cid.max(0) as u32;
            if notdef {
                self.notdefs.push(CidRange {
                    len: width,
                    lo: value,
                    hi: value,
                    cid,
                });
            } else {
                self.singles.insert((width, value), cid);
            }
        }
    }

    /// Reads `<lo> <hi> cid` triples until `endcidrange`/`endnotdefrange`.
    fn parse_cidranges(&mut self, lx: &mut Lexer<'_>, len: usize, notdef: bool) {
        loop {
            let lo = match next_or_skip(lx, len) {
                Some(Token::HexString(h)) => h,
                Some(_) | None => return,
            };
            let Some(Token::HexString(hi)) = next_or_skip(lx, len) else {
                return;
            };
            let Some(Token::Int(cid)) = next_or_skip(lx, len) else {
                return;
            };
            if lo.is_empty() || lo.len() > 4 {
                continue;
            }
            let (lo_v, hi_v) = (code_value(&lo), code_value(&hi));
            if hi_v < lo_v {
                continue;
            }
            let range = CidRange {
                len: lo.len() as u8,
                lo: lo_v,
                hi: hi_v,
                cid: cid.max(0) as u32,
            };
            if notdef {
                self.notdefs.push(range);
            } else {
                self.ranges.push(range);
            }
        }
    }
}

/// How a Type0 font's `/Encoding` maps show-string bytes to CIDs.
pub struct Type0Encoding {
    /// The CMap when one resolved; `None` means the Identity assumption
    /// (2-byte codes, CID == code) — either stated (`/Identity-H`/`-V`) or
    /// the fallback for anything unresolvable.
    pub cmap: Option<Arc<CidCmap>>,
    /// Writing mode 1 (top-to-bottom).
    pub vertical: bool,
    /// False when `/Encoding` named a CMap that could not be resolved (or
    /// was absent), so the Identity fallback is a guess rather than what
    /// the file states.
    pub known: bool,
}

/// Resolves `dict[key]`, treating resolution failures and `null` as absent.
async fn rv<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Object> {
    let obj = dict.get(key)?;
    let resolved = src.resolve(obj).await.ok()?;
    (!resolved.is_null()).then_some(resolved)
}

/// Reads a Type0 font dictionary's `/Encoding` (ISO 32000-1 §9.7.5): the
/// two Identity names map straight through; any other name resolves via
/// [`predefined`]; a stream is parsed as an embedded CMap, its dictionary's
/// `/UseCMap` chain (streams or predefined names, bounded depth) layered
/// underneath and its `/WMode` overriding the content's. Whatever fails
/// resolves to the Identity assumption with `known` false.
pub async fn type0_encoding<S: AsyncObjectSource>(src: &S, font: &Dict) -> Type0Encoding {
    let identity = |vertical: bool, known: bool| Type0Encoding {
        cmap: None,
        vertical,
        known,
    };
    let Some(enc) = rv(src, font, "Encoding").await else {
        return identity(false, false);
    };
    match enc {
        Object::Name(n) if n.0 == "Identity-H" => identity(false, true),
        Object::Name(n) if n.0 == "Identity-V" => identity(true, true),
        Object::Name(n) => match predefined(&n.0) {
            Some(cmap) => Type0Encoding {
                vertical: cmap.vertical(),
                cmap: Some(cmap),
                known: true,
            },
            None => identity(n.0.ends_with("-V"), false),
        },
        Object::Stream(stream) => match embedded_cmap(src, &stream).await {
            Some(cmap) => Type0Encoding {
                vertical: cmap.vertical(),
                cmap: Some(cmap),
                known: true,
            },
            None => identity(false, false),
        },
        _ => identity(false, false),
    }
}

/// Parses an embedded CMap stream with its `/UseCMap` ancestry. `None` when
/// the stream will not read or parses to nothing.
async fn embedded_cmap<S: AsyncObjectSource>(src: &S, stream: &Stream) -> Option<Arc<CidCmap>> {
    // Walk the /UseCMap chain outward first (bounded), then parse from the
    // deepest layer up so each child wraps its parent.
    let mut layers: Vec<(Vec<u8>, Option<i64>)> = Vec::new();
    let mut parent: Option<Arc<CidCmap>> = None;
    let mut current = stream.clone();
    for _ in 0..4 {
        // Through the checked fetch: a CMap labelled with an image codec is
        // a passthrough codestream, refused rather than token-scanned.
        let data = decoded_stream_data_with(src, &current).await.ok()?;
        let wmode = rv(src, &current.dict, "WMode")
            .await
            .and_then(|o| o.as_int());
        layers.push((data, wmode));
        match rv(src, &current.dict, "UseCMap").await {
            Some(Object::Name(n)) => {
                parent = predefined(&n.0);
                break;
            }
            Some(Object::Stream(s)) => current = s,
            _ => break,
        }
    }
    for (data, wmode) in layers.into_iter().rev() {
        let mut resolve = |n: &str| predefined(n);
        let mut cmap = CidCmap::parse_with(&data, parent.take(), &mut resolve);
        if let Some(w) = wmode {
            cmap.wmode = u8::from(w == 1);
        }
        parent = Some(Arc::new(cmap));
    }
    parent.filter(|c| !c.is_empty())
}

/// Fetches the next token, force-advancing past unlexable bytes; `None` at
/// end of input.
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

    /// The codespace block of Adobe's 90ms-RKSJ-H, transcribed from the
    /// BSD-licensed data file: 1-byte and 2-byte ranges interleaved.
    const RKSJ_CODESPACES: &str = "4 begincodespacerange\n\
         <00>   <80>\n\
         <8140> <9FFC>\n\
         <A0>   <DF>\n\
         <E040> <FCFC>\n\
         endcodespacerange\n";

    fn rksj() -> CidCmap {
        let data = format!(
            "{RKSJ_CODESPACES}\
             1 beginnotdefrange\n<00> <1f> 231\nendnotdefrange\n\
             3 begincidrange\n\
             <20> <7d> 231\n\
             <8140> <817e> 633\n\
             <e040> <e07e> 100\n\
             endcidrange\n\
             1 begincidchar\n<a1> 9000\nendcidchar\n"
        );
        CidCmap::parse(data.as_bytes())
    }

    #[test]
    fn rksj_codespaces_split_mixed_widths() {
        let c = rksj();
        // 1-byte ASCII, then a 2-byte code, then 1-byte katakana.
        let bytes = [0x41, 0x81, 0x40, 0xA1];
        assert_eq!(c.code_at(&bytes, 0), (0x41, 1));
        assert_eq!(c.code_at(&bytes, 1), (0x8140, 2));
        assert_eq!(c.code_at(&bytes, 3), (0xA1, 1));
    }

    #[test]
    fn cidrange_arithmetic_offsets_within_the_range() {
        let c = rksj();
        assert_eq!(c.cid(0x20, 1), Some(231));
        assert_eq!(c.cid(0x7d, 1), Some(324));
        assert_eq!(c.cid(0x8140, 2), Some(633));
        assert_eq!(c.cid(0x8163, 2), Some(633 + 0x23));
        assert_eq!(c.cid(0xe041, 2), Some(101));
        assert_eq!(c.cid(0x82FF, 2), None);
    }

    #[test]
    fn cidchar_singletons_map() {
        let c = rksj();
        assert_eq!(c.cid(0xA1, 1), Some(9000));
    }

    #[test]
    fn notdef_ranges_lose_to_real_mappings() {
        let data = format!(
            "{RKSJ_CODESPACES}\
             1 beginnotdefrange\n<00> <1f> 231\nendnotdefrange\n\
             1 begincidrange\n<10> <11> 5\nendcidrange\n"
        );
        let c = CidCmap::parse(data.as_bytes());
        assert_eq!(c.cid(0x10, 1), Some(5)); // real mapping wins
        assert_eq!(c.cid(0x12, 1), Some(231)); // notdef fills the rest
        assert_eq!(c.cid(0x20, 1), None);
    }

    #[test]
    fn a_one_byte_code_and_a_two_byte_code_with_equal_values_stay_apart() {
        let data = "2 begincodespacerange <00> <20> <4000> <41FF> endcodespacerange\n\
                    2 begincidrange <20> <20> 7 <0020> <0020> 9 endcidrange";
        let c = CidCmap::parse(data.as_bytes());
        assert_eq!(c.cid(0x20, 1), Some(7));
        assert_eq!(c.cid(0x20, 2), Some(9));
    }

    #[test]
    fn usecmap_layers_child_over_parent() {
        let parent = Arc::new(CidCmap::parse(
            format!(
                "{RKSJ_CODESPACES}\
                 2 begincidrange <8140> <817e> 633 <20> <7d> 231 endcidrange"
            )
            .as_bytes(),
        ));
        let mut resolve = |name: &str| (name == "90ms-RKSJ-H").then(|| Arc::clone(&parent));
        let child = CidCmap::parse_with(
            b"/90ms-RKSJ-H usecmap\n\
              /WMode 1 def\n\
              1 begincidrange <8141> <8142> 7887 endcidrange",
            None,
            &mut resolve,
        );
        assert!(child.vertical());
        assert_eq!(child.cid(0x8141, 2), Some(7887)); // the vertical variant
        assert_eq!(child.cid(0x8140, 2), Some(633)); // inherited
        assert_eq!(child.cid(0x21, 1), Some(232)); // inherited
        assert_eq!(child.code_at(&[0x81, 0x40], 0), (0x8140, 2)); // codespaces inherited
        assert_eq!(child.parent().map(|p| p.cid(0x8141, 2)), Some(Some(634)));
    }

    #[test]
    fn wmode_reads_and_defaults_horizontal() {
        assert!(!CidCmap::parse(b"/WMode 0 def").vertical());
        assert!(CidCmap::parse(b"/WMode 1 def").vertical());
        assert!(!CidCmap::parse(b"").vertical());
        assert!(CidCmap::identity(true).vertical());
    }

    #[test]
    fn identity_maps_code_to_cid() {
        let c = CidCmap::identity(false);
        assert_eq!(c.code_at(&[0x12, 0x34], 0), (0x1234, 2));
        assert_eq!(c.cid(0x1234, 2), Some(0x1234));
        assert!(!c.single_byte(0x20));
    }

    #[test]
    fn word_spacing_evidence_is_a_one_byte_codespace() {
        assert!(rksj().single_byte(0x20));
        assert!(!rksj().single_byte(0x81));
    }

    /// The never-stall invariant: whatever the bytes, `code_at` consumes at
    /// least one and never reads past the end.
    #[test]
    fn splitting_always_consumes_at_least_one_byte() {
        let cmaps = [rksj(), CidCmap::identity(false), CidCmap::parse(b"")];
        for c in &cmaps {
            for bytes in [&[0x81][..], &[0xFF][..], &[0x00, 0x81][..]] {
                let mut pos = 0;
                let mut codes = 0;
                while pos < bytes.len() {
                    let (_, n) = c.code_at(bytes, pos);
                    assert!(n >= 1);
                    pos += usize::from(n).min(bytes.len() - pos);
                    codes += 1;
                }
                assert!(codes >= 1);
            }
        }
        // A truncated 2-byte tail folds what is there.
        assert_eq!(rksj().code_at(&[0x81], 0), (0x81, 1));
    }

    #[test]
    fn a_truncated_section_keeps_what_parsed_so_far() {
        let c = CidCmap::parse(b"2 begincidrange <20> <7d> 231 <8140> <81");
        assert_eq!(c.cid(0x20, 1), Some(231));
        assert_eq!(c.cid(0x8140, 2), None);
        let garbage = CidCmap::parse(b"\xFF\xFE ) ] >> begincidchar <41> 12 endcidchar");
        assert_eq!(garbage.cid(0x41, 1), Some(12));
        assert!(CidCmap::parse(b"").is_empty());
    }
}
