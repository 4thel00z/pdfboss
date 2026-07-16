//! Cross-reference loading (ISO 32000 §7.5): classic tables, xref streams,
//! hybrid files (`/XRefStm`), `/Prev` chains, and a whole-file recovery scan
//! when everything else fails.

use crate::hash::{FastMap, FastSet};

use crate::error::{Error, Result};
use crate::filters::{decode_stream, is_pdf_whitespace};
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::parser::{NoResolve, Parser};

/// One cross-reference table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// A free entry.
    Free,
    /// An object stored directly in the file at `offset`.
    InFile { offset: u64, gen: u16 },
    /// An object stored inside object stream `stream_num` at `index`.
    InStream { stream_num: u32, index: u32 },
}

/// The merged cross-reference table plus the (merged) trailer dictionary.
#[derive(Debug, Clone, Default)]
pub struct Xref {
    map: FastMap<u32, XrefEntry>,
    /// Merged trailer dictionary; keys from newer sections win.
    pub trailer: Dict,
}

impl Xref {
    /// Looks up the entry for object number `num`.
    pub fn get(&self, num: u32) -> Option<XrefEntry> {
        self.map.get(&num).copied()
    }

    /// Inserts `entry` for `num` unless an entry is already present.
    fn add(&mut self, num: u32, entry: XrefEntry) {
        self.map.entry(num).or_insert(entry);
    }

    /// Merges an older section into this one. Entries already present win
    /// (sections are walked newest to oldest, first-seen wins); trailer
    /// keys already present are kept.
    pub fn merge(&mut self, older: Xref) {
        for (num, entry) in older.map {
            self.map.entry(num).or_insert(entry);
        }
        for (key, value) in older.trailer.iter() {
            if self.trailer.get(&key.0).is_none() {
                self.trailer.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Loads the cross-reference data for a whole file: locates `startxref`
/// (last 1 KiB, widening to the last 64 KiB), parses the classic table or
/// xref stream found there, follows `/XRefStm` and `/Prev` with a
/// visited-offset loop guard, and on any failure falls back to a whole-file
/// recovery scan for `N G obj` headers.
pub fn load_xref(data: &[u8]) -> Result<Xref> {
    let chained = find_startxref(data).and_then(|start| load_chain(data, start).ok());
    match chained {
        Some(xref) if xref.trailer.get("Root").is_some() => Ok(xref),
        chained => recovery_scan(data).or_else(|err| chained.ok_or(err)),
    }
}

/// Finds the byte offset announced after the last `startxref` keyword,
/// searching the last 1 KiB first and widening to the last 64 KiB when the
/// keyword is absent from the smaller window.
fn find_startxref(data: &[u8]) -> Option<usize> {
    for window in [1024usize, 64 * 1024] {
        let tail = data.len().saturating_sub(window);
        if let Some(rel) = memchr::memmem::rfind(&data[tail..], b"startxref") {
            let mut lexer = Lexer::at(data, tail + rel + b"startxref".len());
            if let Ok(Token::Int(v)) = lexer.next_token() {
                if let Some(offset) = to_offset(v, data) {
                    return Some(offset);
                }
            }
        }
        if window >= data.len() {
            break;
        }
    }
    None
}

/// Converts an integer file offset to `usize` when it lies inside `data`.
fn to_offset(v: i64, data: &[u8]) -> Option<usize> {
    usize::try_from(v).ok().filter(|&o| o < data.len())
}

/// Walks the section chain newest→oldest starting at `start`, merging every
/// section into one table (first-seen entries win). A classic trailer's
/// `/XRefStm` section (hybrid file) merges ahead of its table — the table
/// marks the stream's objects free to hide them from old readers — and both
/// merge before `/Prev` is followed. Visited offsets guard against loops.
fn load_chain(data: &[u8], start: usize) -> Result<Xref> {
    let mut acc = Xref::default();
    let mut visited: FastSet<usize> = FastSet::default();
    let mut next = Some(start);
    while let Some(off) = next {
        if !visited.insert(off) {
            break;
        }
        let mut lexer = Lexer::at(data, off);
        let classic = matches!(lexer.peek_token(),
                               Ok(Token::Keyword(ref k)) if k.as_slice() == b"xref");
        let prev = if classic {
            let (section, prev, xrefstm) = parse_classic(data, off)?;
            if let Some(xs) = xrefstm.and_then(|v| to_offset(v, data)) {
                if visited.insert(xs) {
                    // Lenient: a broken hybrid stream leaves the table alone.
                    if let Ok((stream_section, _)) = parse_stream_section(data, xs) {
                        acc.merge(stream_section);
                    }
                }
            }
            acc.merge(section);
            prev
        } else {
            let (section, prev) = parse_stream_section(data, off)?;
            acc.merge(section);
            prev
        };
        next = prev.and_then(|v| to_offset(v, data));
    }
    if acc.map.is_empty() {
        Err(Error::InvalidXref)
    } else {
        Ok(acc)
    }
}

/// Big-endian integer from up to 8 bytes; an empty slice reads as 0.
fn read_be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |acc, &b| (acc << 8) | u64::from(b))
}

/// Parses a classic `xref` section at `off`: `start count` subsection
/// headers, then `count` entries each of `offset gen n|f`, ending with
/// `trailer` and its dictionary. Entries are read token-wise, so malformed
/// 19- or 21-byte entry lines load just as well as conforming 20-byte ones.
/// Returns the section plus the trailer's `/Prev` and `/XRefStm` values.
fn parse_classic(data: &[u8], off: usize) -> Result<(Xref, Option<i64>, Option<i64>)> {
    let mut lexer = Lexer::at(data, off);
    match lexer.next_token()? {
        Token::Keyword(ref k) if k.as_slice() == b"xref" => {}
        _ => return Err(Error::InvalidXref),
    }
    let mut section = Xref::default();
    loop {
        match lexer.next_token()? {
            Token::Int(start) if start >= 0 => {
                let count = match lexer.next_token()? {
                    Token::Int(c) if c >= 0 => c as u64,
                    _ => return Err(Error::InvalidXref),
                };
                // Even a degenerate entry line needs at least 11 bytes, so
                // a count beyond this bound cannot be real.
                if count > data.len() as u64 / 11 + 1 {
                    return Err(Error::InvalidXref);
                }
                for i in 0..count {
                    let f1 = match lexer.next_token()? {
                        Token::Int(v) if v >= 0 => v as u64,
                        _ => return Err(Error::InvalidXref),
                    };
                    let f2 = match lexer.next_token()? {
                        Token::Int(v) if v >= 0 => v,
                        _ => return Err(Error::InvalidXref),
                    };
                    let entry = match lexer.next_token()? {
                        Token::Keyword(ref k) if k.as_slice() == b"n" => XrefEntry::InFile {
                            offset: f1,
                            gen: f2.min(65535) as u16,
                        },
                        Token::Keyword(ref k) if k.as_slice() == b"f" => XrefEntry::Free,
                        _ => return Err(Error::InvalidXref),
                    };
                    if let Ok(num) = u32::try_from(start as u64 + i) {
                        section.add(num, entry);
                    }
                }
            }
            Token::Keyword(ref k) if k.as_slice() == b"trailer" => {
                let mut parser = Parser::at(data, lexer.pos());
                let trailer = match parser.parse_object(&NoResolve)? {
                    Object::Dict(d) => d,
                    _ => return Err(Error::InvalidXref),
                };
                let prev = trailer.get_int("Prev");
                let xrefstm = trailer.get_int("XRefStm");
                section.trailer = trailer;
                return Ok((section, prev, xrefstm));
            }
            _ => return Err(Error::InvalidXref),
        }
    }
}

/// Parses a cross-reference stream section (`/Type /XRef`) at `off`. The
/// decoded data holds fixed-width big-endian fields laid out per `/W`; a
/// zero-width type field defaults to type 1, `/Index` defaults to
/// `[0 Size]`, and the stream's own dictionary is the section trailer.
/// Returns the section plus the trailer's `/Prev` value.
fn parse_stream_section(data: &[u8], off: usize) -> Result<(Xref, Option<i64>)> {
    let mut parser = Parser::at(data, off);
    let (_, obj) = parser.parse_indirect(&NoResolve)?;
    let stream = match obj {
        Object::Stream(s) => s,
        _ => return Err(Error::InvalidXref),
    };
    let decoded = decode_stream(&stream, &NoResolve).map_err(|_| Error::InvalidXref)?;
    let dict = stream.dict;
    let widths: Vec<usize> = dict
        .get_array("W")
        .ok_or(Error::InvalidXref)?
        .iter()
        .map(|v| {
            v.as_int()
                .filter(|&n| (0..=8).contains(&n))
                .map(|n| n as usize)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(Error::InvalidXref)?;
    let w1 = widths.first().copied().unwrap_or(0);
    let w2 = widths.get(1).copied().unwrap_or(0);
    let w3 = widths.get(2).copied().unwrap_or(0);
    let entry_len = w1 + w2 + w3;
    if entry_len == 0 {
        return Err(Error::InvalidXref);
    }
    let size = dict.get_int("Size").unwrap_or(0).max(0) as u64;
    let subsections: Vec<(u64, u64)> = match dict.get_array("Index") {
        Some(index) => index
            .chunks(2)
            .filter_map(|pair| {
                let start = pair.first()?.as_int()?;
                let count = pair.get(1)?.as_int()?;
                (start >= 0 && count >= 0).then_some((start as u64, count as u64))
            })
            .collect(),
        None => vec![(0, size)],
    };
    let mut section = Xref::default();
    let mut pos = 0usize;
    'subsections: for (start, count) in subsections {
        for i in 0..count {
            if pos + entry_len > decoded.len() {
                break 'subsections; // lenient: truncated data ends the table
            }
            let kind = if w1 == 0 {
                1
            } else {
                read_be(&decoded[pos..pos + w1])
            };
            let f2 = read_be(&decoded[pos + w1..pos + w1 + w2]);
            let f3 = read_be(&decoded[pos + w1 + w2..pos + entry_len]);
            pos += entry_len;
            let entry = match kind {
                1 => XrefEntry::InFile {
                    offset: f2,
                    gen: f3.min(65535) as u16,
                },
                2 => match (u32::try_from(f2), u32::try_from(f3)) {
                    (Ok(stream_num), Ok(index)) => XrefEntry::InStream { stream_num, index },
                    _ => XrefEntry::Free,
                },
                // Type 0 is free; unknown types read as references to the
                // null object, which a free entry models exactly.
                _ => XrefEntry::Free,
            };
            if let Ok(num) = u32::try_from(start + i) {
                section.add(num, entry);
            }
        }
    }
    let prev = dict.get_int("Prev");
    section.trailer = dict;
    Ok((section, prev))
}

/// Whole-file recovery: collects every `N G obj` header (the last
/// occurrence of an object number wins), adopts the last parseable trailer
/// dictionary (preferring one that names `/Root`), and failing that
/// promotes the first `/Type /Catalog` object found to `/Root`.
fn recovery_scan(data: &[u8]) -> Result<Xref> {
    let mut xref = Xref::default();
    for pos in memchr::memmem::find_iter(data, b"obj") {
        if let Some((num, gen, start)) = obj_header_before(data, pos) {
            // Direct insert: a later definition of the same object wins.
            xref.map.insert(
                num,
                XrefEntry::InFile {
                    offset: start as u64,
                    gen,
                },
            );
        }
    }
    if xref.map.is_empty() {
        return Err(Error::InvalidXref);
    }
    let trailers: Vec<usize> = memchr::memmem::find_iter(data, b"trailer").collect();
    for &tp in trailers.iter().rev() {
        let mut parser = Parser::at(data, tp + b"trailer".len());
        if let Ok(Object::Dict(dict)) = parser.parse_object(&NoResolve) {
            let has_root = dict.get("Root").is_some();
            if xref.trailer.is_empty() || has_root {
                xref.trailer = dict;
            }
            if has_root {
                break;
            }
        }
    }
    if xref.trailer.get("Root").is_none() {
        if let Some(catalog) = find_catalog(data, &xref) {
            xref.trailer
                .insert(Name("Root".to_string()), Object::Ref(catalog));
        }
    }
    if xref.trailer.get("Size").is_none() {
        let size = xref.map.keys().max().map_or(0, |&m| i64::from(m) + 1);
        xref.trailer
            .insert(Name("Size".to_string()), Object::Int(size));
    }
    Ok(xref)
}

/// Parses recovered objects in ascending number order until one turns out
/// to be a dictionary with `/Type /Catalog`; returns its reference.
fn find_catalog(data: &[u8], xref: &Xref) -> Option<ObjRef> {
    let mut nums: Vec<u32> = xref.map.keys().copied().collect();
    nums.sort_unstable();
    for num in nums {
        let XrefEntry::InFile { offset, .. } = xref.map[&num] else {
            continue;
        };
        let mut parser = Parser::at(data, offset as usize);
        let Ok((r, obj)) = parser.parse_indirect(&NoResolve) else {
            continue;
        };
        let is_catalog = obj
            .as_dict()
            .and_then(|d| d.get_name("Type"))
            .is_some_and(|n| n.0 == "Catalog");
        if is_catalog {
            return Some(r);
        }
    }
    None
}

/// If the `obj` keyword at byte `kw` terminates an `N G obj` header,
/// returns `(N, G, header start offset)`. Both numbers must be plain digit
/// runs at token boundaries, separated from each other and from `obj` by at
/// least one whitespace byte, with values in range for `u32`/`u16`.
fn obj_header_before(data: &[u8], kw: usize) -> Option<(u32, u16, usize)> {
    if let Some(&after) = data.get(kw + 3) {
        if !is_token_boundary(after) {
            return None;
        }
    }
    let gen_end = strip_ws_back(data, kw);
    let gen_start = strip_digits_back(data, gen_end);
    let num_end = strip_ws_back(data, gen_start);
    let num_start = strip_digits_back(data, num_end);
    if gen_end == kw || gen_start == gen_end || num_end == gen_start || num_start == num_end {
        return None;
    }
    if num_start > 0 && !is_token_boundary(data[num_start - 1]) {
        return None;
    }
    let gen: u16 = ascii_int(&data[gen_start..gen_end])?;
    let num: u32 = ascii_int(&data[num_start..num_end])?;
    Some((num, gen, num_start))
}

/// Steps `end` back over trailing PDF whitespace.
fn strip_ws_back(data: &[u8], mut end: usize) -> usize {
    while end > 0 && is_pdf_whitespace(data[end - 1]) {
        end -= 1;
    }
    end
}

/// Steps `end` back over trailing ASCII digits.
fn strip_digits_back(data: &[u8], mut end: usize) -> usize {
    while end > 0 && data[end - 1].is_ascii_digit() {
        end -= 1;
    }
    end
}

/// Parses a decimal integer from ASCII digits, rejecting overflow.
fn ascii_int<T: std::str::FromStr>(bytes: &[u8]) -> Option<T> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// True for bytes that end a token: PDF whitespace or a delimiter.
fn is_token_boundary(b: u8) -> bool {
    is_pdf_whitespace(b)
        || matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_testkit::{objstm_payload, simple_doc, PdfBuilder};

    /// Offset of the first occurrence of `needle` in `data`.
    fn pos_of(data: &[u8], needle: &[u8]) -> usize {
        memchr::memmem::find(data, needle).unwrap()
    }

    /// Asserts that object `num` is an in-file entry pointing at its own
    /// `num 0 obj` header.
    fn assert_points_at_header(xref: &Xref, data: &[u8], num: u32) {
        let header = format!("{num} 0 obj");
        let expected = pos_of(data, header.as_bytes()) as u64;
        assert_eq!(
            xref.get(num),
            Some(XrefEntry::InFile {
                offset: expected,
                gen: 0
            }),
            "entry for object {num}"
        );
    }

    /// Overwrites the digits after the last `startxref` with nines so the
    /// announced offset points nowhere useful.
    fn corrupt_startxref(data: &mut [u8]) {
        let mut i = memchr::memmem::rfind(data, b"startxref").unwrap() + b"startxref".len();
        while !data[i].is_ascii_digit() {
            i += 1;
        }
        while data[i].is_ascii_digit() {
            data[i] = b'9';
            i += 1;
        }
    }

    #[test]
    fn classic_table_loads_all_entries() {
        let data = simple_doc("Hello");
        let xref = load_xref(&data).unwrap();
        assert_eq!(xref.get(0), Some(XrefEntry::Free));
        for num in 1..=5 {
            assert_points_at_header(&xref, &data, num);
        }
        assert_eq!(xref.get(6), None);
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
        assert_eq!(xref.trailer.get_int("Size"), Some(6));
    }

    #[test]
    fn xref_stream_loads_infile_and_instream_entries() {
        let (dict, payload) = objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>"),
            (5, "(text)"),
        ]);
        let mut b = PdfBuilder::new();
        b.stream(6, &dict, &payload);
        b.stream(4, "", b"BT ET");
        let data = b.build_xref_stream(1);
        let xref = load_xref(&data).unwrap();
        assert_eq!(xref.get(0), Some(XrefEntry::Free));
        for (num, index) in [(1, 0), (2, 1), (5, 2)] {
            assert_eq!(
                xref.get(num),
                Some(XrefEntry::InStream {
                    stream_num: 6,
                    index
                }),
                "type-2 entry for object {num}"
            );
        }
        for num in [4, 6, 7] {
            assert_points_at_header(&xref, &data, num);
        }
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
        assert_eq!(
            xref.trailer.get_name("Type").map(|n| n.0.as_str()),
            Some("XRef"),
            "the stream's own dictionary is the trailer"
        );
    }

    #[test]
    fn recovery_scan_after_corrupt_startxref() {
        let mut data = simple_doc("rescue me");
        corrupt_startxref(&mut data);
        let xref = load_xref(&data).unwrap();
        for num in 1..=5 {
            assert_points_at_header(&xref, &data, num);
        }
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
    }

    #[test]
    fn recovery_finds_root_via_catalog_when_trailer_is_unreadable() {
        let mut data = simple_doc("x");
        corrupt_startxref(&mut data);
        let tp = memchr::memmem::rfind(&data, b"trailer").unwrap();
        data[tp..tp + b"trailer".len()].copy_from_slice(b"trai1er");
        let xref = load_xref(&data).unwrap();
        for num in 1..=5 {
            assert_points_at_header(&xref, &data, num);
        }
        let root = xref.trailer.get_ref("Root").unwrap();
        assert_eq!((root.num, root.gen), (1, 0), "object 1 is the catalog");
        assert_eq!(xref.trailer.get_int("Size"), Some(6), "synthesized /Size");
    }

    #[test]
    fn recovery_last_occurrence_of_an_object_wins() {
        let mut data = simple_doc("x");
        corrupt_startxref(&mut data);
        let redefinition = data.len() as u64;
        data.extend_from_slice(b"5 0 obj\n<< /Replaced true >>\nendobj\n");
        let xref = load_xref(&data).unwrap();
        assert_eq!(
            xref.get(5),
            Some(XrefEntry::InFile {
                offset: redefinition,
                gen: 0
            })
        );
        for num in 1..=4 {
            assert_points_at_header(&xref, &data, num);
        }
    }

    #[test]
    fn recovery_ignores_endobj_and_bad_headers() {
        let data = b"garbage endobj more\n7 2 obj\n<< /Type /Catalog >>\nendobj\nxobj 9 9";
        let xref = load_xref(data).unwrap();
        let offset = pos_of(data, b"7 2 obj") as u64;
        assert_eq!(xref.get(7), Some(XrefEntry::InFile { offset, gen: 2 }));
        assert_eq!(xref.map.len(), 1, "only the real header is recovered");
        let root = xref.trailer.get_ref("Root").unwrap();
        assert_eq!((root.num, root.gen), (7, 2));
    }

    #[test]
    fn unrecoverable_garbage_is_invalid_xref() {
        assert!(matches!(load_xref(b"not a pdf"), Err(Error::InvalidXref)));
        assert!(matches!(load_xref(b""), Err(Error::InvalidXref)));
    }

    #[test]
    fn classic_table_with_19_and_21_byte_lines() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let obj2 = data.len();
        data.extend_from_slice(b"2 0 obj\n(hi)\nendobj\n");
        let xref_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n");
        data.extend_from_slice(b"0000000000 65535 f\n"); // 19 bytes: bare LF
        data.extend_from_slice(format!("{obj1:010} 00000 n\n").as_bytes()); // 19 bytes
        data.extend_from_slice(format!("{obj2:010} 00000  n\r\n").as_bytes()); // 21 bytes
        data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
        data.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
        let xref = load_xref(&data).unwrap();
        // Entry 0 proves the table itself was read (recovery never adds Free).
        assert_eq!(xref.get(0), Some(XrefEntry::Free));
        for (num, offset) in [(1, obj1), (2, obj2)] {
            assert_eq!(
                xref.get(num),
                Some(XrefEntry::InFile {
                    offset: offset as u64,
                    gen: 0
                })
            );
        }
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
    }

    #[test]
    fn xref_stream_prev_chains_to_classic_section() {
        let mut data = b"%PDF-1.5\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let obj2_old = data.len();
        data.extend_from_slice(b"2 0 obj\n(old)\nendobj\n");
        let classic_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(format!("{obj2_old:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
        // Incremental update: object 2 replaced, object 3 added.
        let obj2_new = data.len();
        data.extend_from_slice(b"2 0 obj\n(new)\nendobj\n");
        let obj3 = data.len();
        data.extend_from_slice(b"3 0 obj\n42\nendobj\n");
        let stream_off = data.len();
        let mut fields = Vec::new();
        for offset in [obj2_new, obj3, stream_off] {
            fields.push(1u8);
            fields.extend_from_slice(&(offset as u32).to_be_bytes());
            fields.extend_from_slice(&0u16.to_be_bytes());
        }
        data.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Index [2 3] \
                 /Prev {} /Root 1 0 R /Length {} >>\nstream\n",
                classic_off,
                fields.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&fields);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        data.extend_from_slice(format!("startxref\n{stream_off}\n%%EOF\n").as_bytes());

        let xref = load_xref(&data).unwrap();
        assert_eq!(xref.get(0), Some(XrefEntry::Free));
        let infile = |offset: usize| {
            Some(XrefEntry::InFile {
                offset: offset as u64,
                gen: 0,
            })
        };
        assert_eq!(xref.get(1), infile(obj1), "only the classic section has 1");
        assert_eq!(xref.get(2), infile(obj2_new), "newest section wins");
        assert_eq!(xref.get(3), infile(obj3));
        assert_eq!(xref.get(4), infile(stream_off));
        assert_eq!(xref.trailer.get_int("Size"), Some(5), "newest trailer wins");
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
    }

    #[test]
    fn hybrid_xrefstm_overrides_entries_the_table_marks_free() {
        let mut data = b"%PDF-1.5\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let obj2 = data.len();
        data.extend_from_slice(b"2 0 obj\n(hidden)\nendobj\n");
        let stm_off = data.len();
        let mut fields = Vec::new();
        for offset in [obj2, stm_off] {
            fields.push(1u8);
            fields.extend_from_slice(&(offset as u32).to_be_bytes());
            fields.extend_from_slice(&0u16.to_be_bytes());
        }
        data.extend_from_slice(
            format!(
                "3 0 obj\n<< /Type /XRef /Size 4 /W [1 4 2] /Index [2 1 3 1] \
                 /Root 1 0 R /Length {} >>\nstream\n",
                fields.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&fields);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        let classic_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(b"0000000000 00001 f\r\n"); // object 2 hidden
        data.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R /XRefStm {stm_off} >>\n").as_bytes(),
        );
        data.extend_from_slice(format!("startxref\n{classic_off}\n%%EOF\n").as_bytes());

        let xref = load_xref(&data).unwrap();
        assert_eq!(
            xref.get(2),
            Some(XrefEntry::InFile {
                offset: obj2 as u64,
                gen: 0
            }),
            "the hybrid stream entry beats the table's free entry"
        );
        assert_eq!(
            xref.get(3),
            Some(XrefEntry::InFile {
                offset: stm_off as u64,
                gen: 0
            })
        );
        assert_eq!(xref.trailer.get_ref("Root").map(|r| r.num), Some(1));
    }

    #[test]
    fn prev_loop_is_broken_by_the_visited_guard() {
        let mut data = b"%PDF-1.4\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_off = data.len();
        data.extend_from_slice(b"xref\n0 2\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R /Prev {xref_off} >>\n").as_bytes(),
        );
        data.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
        let xref = load_xref(&data).unwrap();
        assert_eq!(
            xref.get(1),
            Some(XrefEntry::InFile {
                offset: obj1 as u64,
                gen: 0
            })
        );
    }

    #[test]
    fn startxref_found_beyond_the_last_1_kib() {
        let mut data = simple_doc("padded");
        // A decoy header: only the recovery scan would pick this up.
        data.extend_from_slice(b"999 0 obj\n<< >>\nendobj\n");
        data.extend_from_slice(&vec![b' '; 2048]);
        let xref = load_xref(&data).unwrap();
        assert_eq!(xref.get(999), None, "chain path used, not recovery");
        for num in 1..=5 {
            assert_points_at_header(&xref, &data, num);
        }
    }

    #[test]
    fn merge_keeps_first_seen_entries_and_trailer_keys() {
        let mut newer = Xref::default();
        newer.map.insert(1, XrefEntry::Free);
        newer
            .map
            .insert(2, XrefEntry::InFile { offset: 20, gen: 0 });
        newer
            .trailer
            .insert(Name("Size".to_string()), Object::Int(3));
        let mut older = Xref::default();
        older
            .map
            .insert(1, XrefEntry::InFile { offset: 10, gen: 0 });
        older
            .map
            .insert(3, XrefEntry::InFile { offset: 30, gen: 1 });
        older
            .trailer
            .insert(Name("Size".to_string()), Object::Int(9));
        older
            .trailer
            .insert(Name("Info".to_string()), Object::Int(7));
        newer.merge(older);
        assert_eq!(newer.get(1), Some(XrefEntry::Free), "deletion is kept");
        assert_eq!(newer.get(2), Some(XrefEntry::InFile { offset: 20, gen: 0 }));
        assert_eq!(newer.get(3), Some(XrefEntry::InFile { offset: 30, gen: 1 }));
        assert_eq!(newer.trailer.get_int("Size"), Some(3));
        assert_eq!(newer.trailer.get_int("Info"), Some(7));
    }
}
