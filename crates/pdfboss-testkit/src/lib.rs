//! Internal fixture builder for pdfboss tests: assembles small, well-formed
//! PDF files (classic-xref or xref-stream flavor) with correct offsets, so
//! tests never hand-compute byte positions.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A lightweight reference to an object added to a [`PdfBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjRefLite {
    /// Object number. Fixture objects always use generation 0.
    pub num: u32,
}

/// Incrementally builds a PDF file from raw object bodies.
///
/// Object bodies are stored keyed by object number and serialized in
/// ascending order as `N 0 obj … endobj`. [`PdfBuilder::build`] emits a
/// classic cross-reference table; [`PdfBuilder::build_xref_stream`] emits a
/// `/Type /XRef` cross-reference stream instead.
pub struct PdfBuilder {
    version: (u8, u8),
    objects: BTreeMap<u32, Vec<u8>>,
}

impl PdfBuilder {
    /// Creates a builder for a `%PDF-1.7` file.
    pub fn new() -> Self {
        PdfBuilder {
            version: (1, 7),
            objects: BTreeMap::new(),
        }
    }

    /// Overrides the header version.
    pub fn version(mut self, major: u8, minor: u8) -> Self {
        self.version = (major, minor);
        self
    }

    /// Adds object `num` with `body` given as raw object syntax
    /// (e.g. `"<< /Type /Catalog /Pages 2 0 R >>"`).
    pub fn object(&mut self, num: u32, body: &str) -> ObjRefLite {
        self.objects.insert(num, body.as_bytes().to_vec());
        ObjRefLite { num }
    }

    /// Adds a stream object `num` with extra dictionary entries and data;
    /// `/Length` is computed and added automatically.
    ///
    /// `dict_extra` may be given either as bare entries (`"/Type /ObjStm"`)
    /// or wrapped in `<< … >>`; both merge into the final stream dictionary.
    pub fn stream(&mut self, num: u32, dict_extra: &str, data: &[u8]) -> ObjRefLite {
        let trimmed = dict_extra.trim();
        let inner = trimmed
            .strip_prefix("<<")
            .and_then(|s| s.strip_suffix(">>"))
            .map(str::trim)
            .unwrap_or(trimmed);
        let dict = if inner.is_empty() {
            format!("<< /Length {} >>", data.len())
        } else {
            format!("<< {} /Length {} >>", inner, data.len())
        };
        let mut body = dict.into_bytes();
        body.extend_from_slice(b"\nstream\n");
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.objects.insert(num, body);
        ObjRefLite { num }
    }

    /// Serializes the file with a classic xref table, correct byte offsets,
    /// and a trailer carrying `/Size` and `/Root`.
    ///
    /// The table is a single subsection starting at object 0; entry 0 is the
    /// free-list head (`0000000000 65535 f`) and gaps in the object numbers
    /// become free entries.
    pub fn build(&self, root: u32) -> Vec<u8> {
        let mut out = self.header_bytes();
        let offsets = self.emit_objects(&mut out);
        let size = self.objects.keys().max().copied().unwrap_or(0) + 1;
        let xref_off = out.len();
        out.extend_from_slice(b"xref\n");
        out.extend_from_slice(format!("0 {}\n", size).as_bytes());
        for num in 0..size {
            let line = match offsets.get(&num) {
                Some(&off) => format!("{:010} {:05} n\r\n", off, 0),
                None => format!("{:010} {:05} f\r\n", 0, 65535),
            };
            out.extend_from_slice(line.as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root {} 0 R >>\n", size, root).as_bytes(),
        );
        out.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_off).as_bytes());
        out
    }

    /// Serializes the file using a `/Type /XRef` cross-reference stream
    /// (uncompressed, `/W [1 4 2]`, no `/Filter`) instead of a classic table;
    /// `startxref` points at the stream object.
    ///
    /// Objects whose body is a `/Type /ObjStm` stream are inspected: the
    /// object numbers listed in the object-stream header receive type-2
    /// (in-stream) entries, so fixtures built with [`objstm_payload`]
    /// round-trip through compressed-object lookup.
    pub fn build_xref_stream(&self, root: u32) -> Vec<u8> {
        let mut out = self.header_bytes();
        let offsets = self.emit_objects(&mut out);

        // Objects living inside an object stream get type-2 entries.
        let mut in_stream: BTreeMap<u32, (u32, u16)> = BTreeMap::new();
        for (&num, body) in &self.objects {
            if let Some(contained) = objstm_object_numbers(body) {
                for (index, &objnum) in contained.iter().enumerate() {
                    in_stream.insert(objnum, (num, index as u16));
                }
            }
        }

        let max_regular = self.objects.keys().max().copied().unwrap_or(0);
        let max_contained = in_stream.keys().max().copied().unwrap_or(0);
        let xref_num = max_regular.max(max_contained) + 1;
        let size = xref_num + 1;
        let xref_off = out.len();

        // Entry fields per /W [1 4 2]: 1-byte type, 4-byte big-endian field
        // 2 (offset or containing stream number), 2-byte big-endian field 3
        // (generation or in-stream index).
        let mut data = Vec::with_capacity(size as usize * 7);
        for num in 0..size {
            let (kind, f2, f3): (u8, u32, u16) = if num == 0 {
                (0, 0, 65535)
            } else if num == xref_num {
                (1, xref_off as u32, 0)
            } else if let Some(&off) = offsets.get(&num) {
                (1, off as u32, 0)
            } else if let Some(&(stream_num, index)) = in_stream.get(&num) {
                (2, stream_num, index)
            } else {
                (0, 0, 0)
            };
            data.push(kind);
            data.extend_from_slice(&f2.to_be_bytes());
            data.extend_from_slice(&f3.to_be_bytes());
        }

        let dict = format!(
            "<< /Type /XRef /Size {} /W [1 4 2] /Root {} 0 R /Length {} >>",
            size,
            root,
            data.len()
        );
        out.extend_from_slice(format!("{} 0 obj\n{}\nstream\n", xref_num, dict).as_bytes());
        out.extend_from_slice(&data);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_off).as_bytes());
        out
    }

    /// `%PDF-x.y` header plus a comment line with bytes above 0x7F so file
    /// sniffers treat the fixture as binary.
    fn header_bytes(&self) -> Vec<u8> {
        let mut out = format!("%PDF-{}.{}\n", self.version.0, self.version.1).into_bytes();
        out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
        out
    }

    /// Appends every object as `N 0 obj\n{body}\nendobj\n` in ascending
    /// number order, returning each object's starting byte offset.
    fn emit_objects(&self, out: &mut Vec<u8>) -> BTreeMap<u32, usize> {
        let mut offsets = BTreeMap::new();
        for (&num, body) in &self.objects {
            offsets.insert(num, out.len());
            out.extend_from_slice(format!("{} 0 obj\n", num).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        offsets
    }
}

impl Default for PdfBuilder {
    fn default() -> Self {
        Self::new()
    }
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const FONT_HELVETICA: &str =
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

/// Page dictionary body: US-Letter media box, one `/F1` font resource.
fn page_body(parent: u32, contents: u32, font: u32) -> String {
    format!(
        "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 {} 0 R >> >> /Contents {} 0 R >>",
        parent, font, contents
    )
}

/// Content-stream operators that show `text` at (72, 720) in 12pt `/F1`.
fn show_text_content(text: &str) -> String {
    format!("BT /F1 12 Tf 72 720 Td ({}) Tj ET", escape_text(text))
}

/// Escapes the literal-string delimiters `(`, `)` and `\` for embedding in
/// a PDF literal string.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            _ => out.push(c),
        }
    }
    out
}

/// Catalog(1) → Pages(2) → Page(3) with content stream (4) and font (5).
fn single_page_builder(content: &[u8]) -> PdfBuilder {
    let mut b = PdfBuilder::new();
    b.object(1, CATALOG);
    b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.object(3, &page_body(2, 4, 5));
    b.stream(4, "", content);
    b.object(5, FONT_HELVETICA);
    b
}

/// One-call fixture: a single page showing `text` in 12pt Helvetica.
pub fn simple_doc(text: &str) -> Vec<u8> {
    single_page_builder(show_text_content(text).as_bytes()).build(1)
}

/// One-call fixture: one page per entry in `pages`, each showing its text
/// in 12pt Helvetica.
pub fn multi_page_doc(pages: &[&str]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.object(1, CATALOG);
    let mut kids = String::new();
    for i in 0..pages.len() {
        if i > 0 {
            kids.push(' ');
        }
        write!(kids, "{} 0 R", 4 + 2 * i as u32).unwrap();
    }
    b.object(
        2,
        &format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids, pages.len()),
    );
    b.object(3, FONT_HELVETICA);
    for (i, text) in pages.iter().enumerate() {
        let page_num = 4 + 2 * i as u32;
        b.object(page_num, &page_body(2, page_num + 1, 3));
        b.stream(page_num + 1, "", show_text_content(text).as_bytes());
    }
    b.build(1)
}

/// One-call fixture: a single page whose content stream is `content`
/// verbatim (raw operators). The page still carries the `/F1` Helvetica
/// resource so text operators work too.
pub fn doc_with_graphics(content: &str) -> Vec<u8> {
    single_page_builder(content.as_bytes()).build(1)
}

/// Builds the decoded payload and dictionary entries for an object stream
/// (`/Type /ObjStm`) holding the given `(number, body)` pairs.
///
/// Returns `(dict_extra, payload)`: pass both to [`PdfBuilder::stream`] and
/// serialize with [`PdfBuilder::build_xref_stream`], which emits type-2
/// cross-reference entries for the contained object numbers. Do not also add
/// the contained objects to the builder directly, and note that stream
/// objects may not live inside an object stream.
pub fn objstm_payload(objects: &[(u32, &str)]) -> (String, Vec<u8>) {
    let mut header = String::new();
    let mut bodies: Vec<u8> = Vec::new();
    for (i, (num, body)) in objects.iter().enumerate() {
        if i > 0 {
            header.push(' ');
            bodies.push(b'\n');
        }
        write!(header, "{} {}", num, bodies.len()).unwrap();
        bodies.extend_from_slice(body.as_bytes());
    }
    header.push('\n');
    let first = header.len();
    let dict = format!("/Type /ObjStm /N {} /First {}", objects.len(), first);
    let mut payload = header.into_bytes();
    payload.extend_from_slice(&bodies);
    (dict, payload)
}

/// Finds the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// If `body` is a serialized `/Type /ObjStm` stream object, returns the
/// object numbers listed in its header, in order.
fn objstm_object_numbers(body: &[u8]) -> Option<Vec<u32>> {
    let stream_pos = find(body, b">>\nstream\n")?;
    let dict = std::str::from_utf8(&body[..stream_pos]).ok()?;
    if !dict.contains("/ObjStm") {
        return None;
    }
    let tokens: Vec<&str> = dict.split_whitespace().collect();
    let n_pos = tokens.iter().position(|&t| t == "/N")?;
    let n: usize = tokens.get(n_pos + 1)?.parse().ok()?;
    let payload = &body[stream_pos + b">>\nstream\n".len()..];
    let header = leading_ints(payload, 2 * n)?;
    Some(header.iter().step_by(2).map(|&v| v as u32).collect())
}

/// Parses the first `count` whitespace-separated non-negative integers from
/// `data`.
fn leading_ints(data: &[u8], count: usize) -> Option<Vec<u64>> {
    let mut out = Vec::with_capacity(count);
    let mut i = 0;
    while out.len() < count {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < data.len() && data[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        out.push(std::str::from_utf8(&data[start..i]).ok()?.parse().ok()?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        find(haystack, needle).is_some()
    }

    /// Reads the offset following the last `startxref` keyword.
    fn startxref_offset(bytes: &[u8]) -> usize {
        let pos = bytes
            .windows(b"startxref".len())
            .rposition(|w| w == b"startxref")
            .expect("startxref keyword present");
        let tail = std::str::from_utf8(&bytes[pos + b"startxref".len()..]).unwrap();
        tail.split_whitespace().next().unwrap().parse().unwrap()
    }

    /// Parses a classic xref section at the `startxref` target into
    /// `(num, field1, field2, kind)` tuples.
    fn classic_entries(bytes: &[u8]) -> Vec<(u32, u64, u32, u8)> {
        let off = startxref_offset(bytes);
        assert!(
            bytes[off..].starts_with(b"xref"),
            "startxref target must begin with the xref keyword"
        );
        let text = std::str::from_utf8(&bytes[off..]).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("xref"));
        let sub = lines.next().unwrap();
        let mut parts = sub.split_whitespace();
        let start: u32 = parts.next().unwrap().parse().unwrap();
        let count: u32 = parts.next().unwrap().parse().unwrap();
        let mut entries = Vec::new();
        for i in 0..count {
            let line = lines.next().unwrap();
            let mut fields = line.split_whitespace();
            let f1: u64 = fields.next().unwrap().parse().unwrap();
            let f2: u32 = fields.next().unwrap().parse().unwrap();
            let kind = fields.next().unwrap().as_bytes()[0];
            entries.push((start + i, f1, f2, kind));
        }
        entries
    }

    /// Parses the xref stream at the `startxref` target: returns the stream's
    /// object number, decoded `(type, field2, field3)` entries, and the
    /// startxref offset itself.
    fn xref_stream_entries(bytes: &[u8]) -> (u32, Vec<(u8, u64, u32)>, usize) {
        let off = startxref_offset(bytes);
        let section = &bytes[off..];
        let dict_end = find(section, b">>\nstream\n").expect("xref stream object at offset");
        let head = std::str::from_utf8(&section[..dict_end]).unwrap();
        assert!(head.contains("/Type /XRef"), "missing /Type /XRef: {head}");
        assert!(head.contains("/W [1 4 2]"), "missing /W [1 4 2]: {head}");
        assert!(!head.contains("/Filter"), "xref stream must be raw: {head}");
        let tokens: Vec<&str> = head.split_whitespace().collect();
        let objnum: u32 = tokens[0].parse().unwrap();
        assert_eq!(tokens[1], "0");
        assert_eq!(tokens[2], "obj");
        let size_pos = tokens.iter().position(|&t| t == "/Size").unwrap();
        let size: usize = tokens[size_pos + 1].parse().unwrap();
        let data_start = dict_end + b">>\nstream\n".len();
        let data = &section[data_start..data_start + size * 7];
        let entries = data
            .chunks(7)
            .map(|c| {
                (
                    c[0],
                    u64::from(u32::from_be_bytes([c[1], c[2], c[3], c[4]])),
                    u32::from(u16::from_be_bytes([c[5], c[6]])),
                )
            })
            .collect();
        (objnum, entries, off)
    }

    #[test]
    fn simple_doc_header_and_footer() {
        let bytes = simple_doc("hi");
        assert!(bytes.starts_with(b"%PDF-1.7\n"));
        // Second line is a binary comment: '%' then bytes above 0x7F.
        assert_eq!(bytes[9], b'%');
        assert!(bytes[10] > 0x7F && bytes[11] > 0x7F);
        assert!(bytes.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn version_override_changes_header() {
        let bytes = PdfBuilder::new().version(1, 4).build(1);
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
    }

    #[test]
    fn classic_startxref_points_at_xref_keyword() {
        let bytes = simple_doc("x");
        let off = startxref_offset(&bytes);
        assert_eq!(&bytes[off..off + 5], b"xref\n");
    }

    #[test]
    fn classic_offsets_match_object_positions() {
        for bytes in [
            simple_doc("Hello"),
            multi_page_doc(&["one", "two", "three"]),
            doc_with_graphics("0 0 10 10 re f"),
        ] {
            let entries = classic_entries(&bytes);
            assert_eq!(entries[0], (0, 0, 65535, b'f'), "free-list head entry");
            let mut in_use = 0;
            for &(num, offset, gen, kind) in &entries[1..] {
                if kind != b'n' {
                    continue;
                }
                in_use += 1;
                assert_eq!(gen, 0);
                let header = format!("{} 0 obj", num);
                assert!(
                    bytes[offset as usize..].starts_with(header.as_bytes()),
                    "entry for object {num} points at wrong offset {offset}"
                );
            }
            assert!(in_use >= 5, "expected at least 5 in-use objects");
        }
    }

    #[test]
    fn classic_entries_are_exactly_20_bytes() {
        let bytes = simple_doc("x");
        let off = startxref_offset(&bytes);
        let section = &bytes[off..];
        let after_kw = find(section, b"\n").unwrap() + 1;
        let sub_len = find(&section[after_kw..], b"\n").unwrap() + 1;
        let sub_line = std::str::from_utf8(&section[after_kw..after_kw + sub_len - 1]).unwrap();
        let count: usize = sub_line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let table = after_kw + sub_len;
        for i in 0..count {
            let entry = &section[table + i * 20..table + (i + 1) * 20];
            assert_eq!(entry[10], b' ');
            assert_eq!(entry[16], b' ');
            assert!(entry[17] == b'n' || entry[17] == b'f');
            assert_eq!(&entry[18..], b"\r\n");
            assert!(entry[..10].iter().all(u8::is_ascii_digit));
            assert!(entry[11..16].iter().all(u8::is_ascii_digit));
        }
        assert!(section[table + count * 20..].starts_with(b"trailer"));
    }

    #[test]
    fn classic_trailer_has_size_and_root() {
        let bytes = simple_doc("x");
        assert!(contains(&bytes, b"trailer\n<< /Size 6 /Root 1 0 R >>\n"));
    }

    #[test]
    fn gaps_become_free_entries() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog >>");
        b.object(3, "<< /X 1 >>");
        let bytes = b.build(1);
        let entries = classic_entries(&bytes);
        assert_eq!(entries.len(), 4); // 0..=3
        assert_eq!(entries[2].3, b'f', "gap object 2 must be free");
        assert_eq!(entries[1].3, b'n');
        assert_eq!(entries[3].3, b'n');
        assert!(contains(&bytes, b"/Size 4"));
    }

    #[test]
    fn object_and_stream_return_refs() {
        let mut b = PdfBuilder::new();
        assert_eq!(b.object(7, "null").num, 7);
        assert_eq!(b.stream(9, "", b"x").num, 9);
    }

    #[test]
    fn stream_computes_length_and_wraps_data() {
        let mut b = PdfBuilder::new();
        b.stream(1, "/Type /Test", b"hello");
        let bytes = b.build(1);
        assert!(contains(
            &bytes,
            b"<< /Type /Test /Length 5 >>\nstream\nhello\nendstream"
        ));
    }

    #[test]
    fn stream_accepts_wrapped_dict_extra() {
        let mut b = PdfBuilder::new();
        b.stream(1, "<< /A (x) >>", b"data");
        let bytes = b.build(1);
        assert!(contains(&bytes, b"<< /A (x) /Length 4 >>\nstream\ndata"));
    }

    #[test]
    fn stream_with_empty_dict_extra() {
        let mut b = PdfBuilder::new();
        b.stream(1, "", b"ab");
        let bytes = b.build(1);
        assert!(contains(&bytes, b"<< /Length 2 >>\nstream\nab\nendstream"));
    }

    #[test]
    fn simple_doc_escapes_string_delimiters() {
        let bytes = simple_doc("a(b)c\\d");
        assert!(contains(&bytes, br"(a\(b\)c\\d) Tj"));
    }

    #[test]
    fn escape_text_handles_all_three() {
        assert_eq!(escape_text("plain"), "plain");
        assert_eq!(escape_text("(("), r"\(\(");
        assert_eq!(escape_text(r"a\b"), r"a\\b");
        assert_eq!(escape_text(")("), r"\)\(");
    }

    #[test]
    fn simple_doc_contains_expected_structure() {
        let bytes = simple_doc("Hi there");
        assert!(contains(&bytes, b"/Type /Catalog"));
        assert!(contains(&bytes, b"/Type /Pages"));
        assert!(contains(&bytes, b"/MediaBox [0 0 612 792]"));
        assert!(contains(&bytes, b"/BaseFont /Helvetica"));
        assert!(contains(&bytes, b"/Encoding /WinAnsiEncoding"));
        assert!(contains(&bytes, b"BT /F1 12 Tf 72 720 Td (Hi there) Tj ET"));
    }

    #[test]
    fn multi_page_doc_structure() {
        let bytes = multi_page_doc(&["one", "two", "three"]);
        assert!(contains(&bytes, b"/Count 3"));
        assert!(contains(&bytes, b"/Kids [4 0 R 6 0 R 8 0 R]"));
        for text in [&b"(one) Tj"[..], b"(two) Tj", b"(three) Tj"] {
            assert!(contains(&bytes, text));
        }
    }

    #[test]
    fn multi_page_doc_empty_is_still_well_formed() {
        let bytes = multi_page_doc(&[]);
        assert!(contains(&bytes, b"/Kids [] /Count 0"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        classic_entries(&bytes); // parses cleanly
    }

    #[test]
    fn doc_with_graphics_embeds_content_verbatim() {
        let content = "1 0 0 rg 10 10 50 50 re f";
        let bytes = doc_with_graphics(content);
        let wrapped = format!("stream\n{}\nendstream", content);
        assert!(contains(&bytes, wrapped.as_bytes()));
    }

    #[test]
    fn xref_stream_startxref_points_at_stream_object() {
        let bytes = single_page_builder(b"BT ET").build_xref_stream(1);
        assert!(bytes.starts_with(b"%PDF-1.7\n"));
        assert!(bytes.ends_with(b"%%EOF\n"));
        let (objnum, entries, off) = xref_stream_entries(&bytes);
        let header = format!("{} 0 obj", objnum);
        assert!(bytes[off..].starts_with(header.as_bytes()));
        // Objects 1..=5 plus the xref stream itself as object 6; size 7.
        assert_eq!(objnum, 6);
        assert_eq!(entries.len(), 7);
        assert_eq!(entries[0], (0, 0, 65535), "free-list head");
        // The stream's own entry points at the startxref target.
        assert_eq!(entries[objnum as usize], (1, off as u64, 0));
    }

    #[test]
    fn xref_stream_offsets_match_object_positions() {
        let bytes = single_page_builder(b"BT ET").build_xref_stream(1);
        let (_, entries, _) = xref_stream_entries(&bytes);
        for (num, &(kind, offset, gen)) in entries.iter().enumerate().skip(1) {
            assert_eq!(kind, 1, "object {num} should be a type-1 entry");
            assert_eq!(gen, 0);
            let header = format!("{} 0 obj", num);
            assert!(
                bytes[offset as usize..].starts_with(header.as_bytes()),
                "entry for object {num} points at wrong offset {offset}"
            );
        }
    }

    #[test]
    fn objstm_payload_offsets_point_at_bodies() {
        let objs: &[(u32, &str)] = &[(1, "<< /A 1 >>"), (2, "<< /B 2 >>"), (7, "3.14")];
        let (dict, payload) = objstm_payload(objs);
        assert!(dict.contains("/Type /ObjStm"));
        assert!(dict.contains("/N 3"));
        let tokens: Vec<&str> = dict.split_whitespace().collect();
        let first_pos = tokens.iter().position(|&t| t == "/First").unwrap();
        let first: usize = tokens[first_pos + 1].parse().unwrap();
        let header = leading_ints(&payload, 6).unwrap();
        for (i, &(num, body)) in objs.iter().enumerate() {
            assert_eq!(header[2 * i], u64::from(num));
            let offset = header[2 * i + 1] as usize;
            assert!(
                payload[first + offset..].starts_with(body.as_bytes()),
                "object {num} offset {offset} does not point at its body"
            );
        }
    }

    #[test]
    fn build_xref_stream_emits_type2_entries_for_objstm_contents() {
        let (dict, payload) = objstm_payload(&[
            (1, CATALOG),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, &page_body(2, 4, 5)),
            (5, FONT_HELVETICA),
        ]);
        let mut b = PdfBuilder::new();
        b.stream(6, &dict, &payload);
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET");
        let bytes = b.build_xref_stream(1);
        let (objnum, entries, off) = xref_stream_entries(&bytes);
        assert_eq!(objnum, 7);
        assert_eq!(entries.len(), 8);
        // Contained objects: type 2, containing stream 6, indices in order.
        assert_eq!(entries[1], (2, 6, 0));
        assert_eq!(entries[2], (2, 6, 1));
        assert_eq!(entries[3], (2, 6, 2));
        assert_eq!(entries[5], (2, 6, 3));
        // Regular objects: type 1 at their real offsets.
        for num in [4u32, 6] {
            let (kind, offset, gen) = entries[num as usize];
            assert_eq!((kind, gen), (1, 0));
            let header = format!("{} 0 obj", num);
            assert!(bytes[offset as usize..].starts_with(header.as_bytes()));
        }
        assert_eq!(entries[7], (1, off as u64, 0));
    }

    #[test]
    fn objstm_detection_ignores_ordinary_streams() {
        assert_eq!(
            objstm_object_numbers(b"<< /Length 2 >>\nstream\nab\nendstream"),
            None
        );
        assert_eq!(objstm_object_numbers(b"<< /Type /Catalog >>"), None);
    }
}
