//! The object-level PDF writer: numbered objects in, finished file bytes
//! out. Handles the header, stream `/Length` bookkeeping, optional Flate
//! compression, object streams, both cross-reference styles, the trailer
//! and a deterministic `/ID`.
//!
//! Determinism contract: the same sequence of calls with the same options
//! produces byte-identical output. Nothing here reads clocks or RNGs; the
//! `/ID` derives from a SHA-256 of the emitted body.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdfboss_core::{Dict, Name, ObjRef, Object, Stream};

use crate::error::{Error, Result};
use crate::ser::{serialize_dict, serialize_object};

/// Which cross-reference flavor `finish` emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XrefStyle {
    /// A classic `xref` table with a `trailer` dictionary (readable by
    /// PDF 1.0-era consumers).
    Table,
    /// A cross-reference stream (`/Type /XRef`, PDF 1.5+), the compact
    /// modern form.
    #[default]
    Stream,
}

/// Options governing file emission.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriteOptions {
    /// Cross-reference flavor. Object streams require [`XrefStyle::Stream`].
    pub xref: XrefStyle,
    /// Flate-compress stream data that carries no filter of its own.
    pub compress: bool,
    /// Pack non-stream objects into object streams (only effective with
    /// [`XrefStyle::Stream`]).
    pub object_streams: bool,
    /// PDF version written in the header.
    pub version: (u8, u8),
}

impl Default for WriteOptions {
    fn default() -> WriteOptions {
        WriteOptions {
            xref: XrefStyle::Stream,
            compress: true,
            object_streams: true,
            version: (1, 7),
        }
    }
}

/// Accumulates numbered objects and serializes them into a complete PDF
/// file. Objects are numbered in the order they are first claimed
/// (`put`, `put_stream`, or `reserve`), starting at 1, generation 0.
#[derive(Debug)]
pub struct Writer {
    options: WriteOptions,
    slots: Vec<Slot>,
    info: Option<ObjRef>,
}

/// One numbered object: reserved, or holding its body.
#[derive(Debug)]
enum Slot {
    Reserved,
    Filled(Object),
}

impl Writer {
    /// Creates a writer with the given options.
    pub fn new(options: WriteOptions) -> Writer {
        Writer {
            options,
            slots: Vec::new(),
            info: None,
        }
    }

    /// Claims an object number now to be filled later — for cycles like
    /// the page tree, where children point at a parent not yet built.
    pub fn reserve(&mut self) -> ObjRef {
        self.push(Slot::Reserved)
    }

    /// Adds a complete object and returns its reference.
    pub fn put(&mut self, obj: Object) -> ObjRef {
        self.push(Slot::Filled(obj))
    }

    /// Adds a stream object. `/Length` is computed on emission; when
    /// [`WriteOptions::compress`] is set and `dict` names no `/Filter`,
    /// the data is Flate-compressed and `/Filter /FlateDecode` added.
    pub fn put_stream(&mut self, mut dict: Dict, data: Vec<u8>) -> ObjRef {
        let data = compress_into(&mut dict, data, self.options.compress);
        self.push(Slot::Filled(Object::Stream(Stream { dict, data })))
    }

    /// Adds a stream object without touching its filters — for data that
    /// is already encoded (e.g. a JPEG passed through as `/DCTDecode`,
    /// with `/Filter` set by the caller). `/Length` is still computed.
    pub fn put_stream_raw(&mut self, dict: Dict, data: Vec<u8>) -> ObjRef {
        self.push(Slot::Filled(Object::Stream(Stream { dict, data })))
    }

    /// Fills a previously [`reserve`](Writer::reserve)d object.
    pub fn fill(&mut self, r: ObjRef, obj: Object) -> Result<()> {
        if r.gen != 0 {
            return Err(Error::Other(format!(
                "cannot fill {} {} R: this writer only issues generation 0",
                r.num, r.gen
            )));
        }
        if r.num == 0 || r.num as usize > self.slots.len() {
            return Err(Error::Other(format!(
                "cannot fill {} 0 R: this writer never allocated that object number",
                r.num
            )));
        }
        let slot = &mut self.slots[r.num as usize - 1];
        if matches!(slot, Slot::Filled(_)) {
            return Err(Error::AlreadyFilled(r));
        }
        *slot = Slot::Filled(obj);
        Ok(())
    }

    fn push(&mut self, slot: Slot) -> ObjRef {
        self.slots.push(slot);
        ObjRef {
            num: self.slots.len() as u32,
            gen: 0,
        }
    }

    /// Registers the document information dictionary for the trailer.
    pub fn set_info(&mut self, info: ObjRef) {
        self.info = Some(info);
    }

    /// Serializes everything into a complete PDF file: header with binary
    /// comment, all objects (packed into object streams where options
    /// allow), the cross-reference, and the trailer with `root`, the
    /// registered info dictionary, and a `/ID` pair derived from a
    /// SHA-256 of the emitted body.
    pub fn finish(self, root: ObjRef) -> Result<Vec<u8>> {
        let Writer {
            options,
            slots,
            info,
        } = self;
        let mut bodies = Vec::with_capacity(slots.len());
        for (index, slot) in slots.into_iter().enumerate() {
            match slot {
                Slot::Reserved => {
                    return Err(Error::Unfilled(ObjRef {
                        num: index as u32 + 1,
                        gen: 0,
                    }))
                }
                Slot::Filled(obj) => bodies.push(obj),
            }
        }
        match options.xref {
            XrefStyle::Table => finish_table(options, &bodies, root, info),
            XrefStyle::Stream => finish_stream(options, &bodies, root, info),
        }
    }
}

/// Objects a single object stream may hold before the next one starts.
const OBJSTM_CAPACITY: usize = 200;

/// One cross-reference row for the stream flavor: a top-level object at a
/// byte offset (type 1) or an object packed into an object stream (type 2).
#[derive(Clone, Copy)]
enum Row {
    Top(u32),
    Packed { container: u32, index: u16 },
}

/// Emits the classic-table flavor: bodies, `xref` table, `trailer`
/// dictionary, `startxref` and `%%EOF`.
fn finish_table(
    options: WriteOptions,
    bodies: &[Object],
    root: ObjRef,
    info: Option<ObjRef>,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_header(&mut out, options.version);
    let mut offsets = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        write_indirect(&mut out, index as u32 + 1, body)?;
    }
    let id = file_id(&out);
    let xref_off = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", bodies.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f\r\n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n\r\n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n");
    let trailer = trailer_dict(bodies.len() as i64 + 1, root, info, id);
    serialize_dict(&trailer, &mut out)?;
    out.extend_from_slice(format!("\nstartxref\n{xref_off}\n%%EOF").as_bytes());
    Ok(out)
}

/// Emits the cross-reference-stream flavor: bodies (non-stream objects
/// packed into object streams when the option is set), then a `/Type /XRef`
/// stream as the last object, `startxref` and `%%EOF`. Object numbering is
/// dense from 0 through the xref stream itself, so `/Index` is never
/// needed.
fn finish_stream(
    options: WriteOptions,
    bodies: &[Object],
    root: ObjRef,
    info: Option<ObjRef>,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_header(&mut out, options.version);
    let user_count = bodies.len() as u32;

    let packed: Vec<u32> = if options.object_streams {
        bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| !matches!(body, Object::Stream(_)))
            .map(|(index, _)| index as u32 + 1)
            .collect()
    } else {
        Vec::new()
    };
    let chunks: Vec<&[u32]> = packed.chunks(OBJSTM_CAPACITY).collect();

    let mut rows: Vec<Row> = vec![Row::Top(0); bodies.len()];
    for (c, chunk) in chunks.iter().enumerate() {
        for (index, num) in chunk.iter().enumerate() {
            rows[*num as usize - 1] = Row::Packed {
                container: user_count + c as u32 + 1,
                index: index as u16,
            };
        }
    }

    for (index, body) in bodies.iter().enumerate() {
        if matches!(rows[index], Row::Packed { .. }) {
            continue;
        }
        rows[index] = Row::Top(field_offset(out.len())?);
        write_indirect(&mut out, index as u32 + 1, body)?;
    }

    let mut container_offsets = Vec::with_capacity(chunks.len());
    for (c, chunk) in chunks.iter().enumerate() {
        let pairs: Vec<(u32, &Object)> = chunk
            .iter()
            .map(|&num| (num, &bodies[num as usize - 1]))
            .collect();
        let container = build_objstm(&pairs, options.compress)?;
        container_offsets.push(field_offset(out.len())?);
        write_indirect(
            &mut out,
            user_count + c as u32 + 1,
            &Object::Stream(container),
        )?;
    }

    let xref_num = user_count + chunks.len() as u32 + 1;
    let id = file_id(&out);
    let xref_off = field_offset(out.len())?;

    let mut data = Vec::with_capacity(7 * (xref_num as usize + 1));
    push_row(&mut data, 0, 0, 65535);
    for row in rows {
        match row {
            Row::Top(offset) => push_row(&mut data, 1, offset, 0),
            Row::Packed { container, index } => push_row(&mut data, 2, container, index),
        }
    }
    for offset in container_offsets {
        push_row(&mut data, 1, offset, 0);
    }
    push_row(&mut data, 1, xref_off, 0);

    let mut dict = trailer_dict(xref_num as i64 + 1, root, info, id);
    dict.insert(literal("Type"), Object::Name(literal("XRef")));
    dict.insert(
        literal("W"),
        Object::Array(vec![Object::Int(1), Object::Int(4), Object::Int(2)]),
    );
    let data = compress_into(&mut dict, data, options.compress);
    write_indirect(&mut out, xref_num, &Object::Stream(Stream { dict, data }))?;
    out.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF").as_bytes());
    Ok(out)
}

/// `%PDF-M.m` plus the binary comment marking the file as 8-bit data.
fn write_header(out: &mut Vec<u8>, version: (u8, u8)) {
    out.extend_from_slice(format!("%PDF-{}.{}\n", version.0, version.1).as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
}

/// Emits `num 0 obj` through `endobj`. Every top-level `Object::Stream` —
/// however it entered the writer — is framed as a stream with a direct
/// `/Length` of its stored byte count; everything else serializes through
/// [`crate::ser`].
fn write_indirect(out: &mut Vec<u8>, num: u32, obj: &Object) -> Result<()> {
    out.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
    match obj {
        Object::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.insert(literal("Length"), Object::Int(s.data.len() as i64));
            serialize_dict(&dict, out)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.data);
            out.extend_from_slice(b"\nendstream");
        }
        direct => serialize_object(direct, out)?,
    }
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

/// Serializes one object-stream container from `(num, body)` pairs: `2·N`
/// header integers, then the bodies, each followed by a space so adjacent
/// tokens cannot fuse.
fn build_objstm(pairs: &[(u32, &Object)], compress: bool) -> Result<Stream> {
    let mut header = Vec::new();
    let mut payload = Vec::new();
    for (num, body) in pairs {
        header.extend_from_slice(format!("{num} {} ", payload.len()).as_bytes());
        serialize_object(body, &mut payload)?;
        payload.push(b' ');
    }
    let mut dict = Dict::new();
    dict.insert(literal("Type"), Object::Name(literal("ObjStm")));
    dict.insert(literal("N"), Object::Int(pairs.len() as i64));
    dict.insert(literal("First"), Object::Int(header.len() as i64));
    header.extend_from_slice(&payload);
    let data = compress_into(&mut dict, header, compress);
    Ok(Stream { dict, data })
}

/// The shared trailer entries: `/Size`, `/Root`, the optional `/Info`, and
/// the `/ID` pair.
fn trailer_dict(size: i64, root: ObjRef, info: Option<ObjRef>, id: Object) -> Dict {
    let mut trailer = Dict::new();
    trailer.insert(literal("Size"), Object::Int(size));
    trailer.insert(literal("Root"), Object::Ref(root));
    if let Some(info) = info {
        trailer.insert(literal("Info"), Object::Ref(info));
    }
    trailer.insert(literal("ID"), id);
    trailer
}

/// The `/ID` array: two identical 16-byte strings from a SHA-256 of every
/// byte emitted before the cross-reference.
fn file_id(emitted: &[u8]) -> Object {
    let digest = pdfboss_core::crypt::sha256(emitted);
    let id = Object::String(digest[..16].to_vec());
    Object::Array(vec![id.clone(), id])
}

/// Flate-compresses `data` and records `/Filter /FlateDecode` in `dict`
/// when `compress` is set and the dictionary names no filter of its own;
/// otherwise the data passes through untouched.
fn compress_into(dict: &mut Dict, data: Vec<u8>, compress: bool) -> Vec<u8> {
    if !compress || dict.get("Filter").is_some() {
        return data;
    }
    dict.insert(literal("Filter"), Object::Name(literal("FlateDecode")));
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&data)
        .expect("writing into a Vec cannot fail");
    encoder
        .finish()
        .expect("finishing an in-memory zlib stream cannot fail")
}

/// One `[1 4 2]` cross-reference-stream row.
fn push_row(rows: &mut Vec<u8>, kind: u8, second: u32, third: u16) {
    rows.push(kind);
    rows.extend_from_slice(&second.to_be_bytes());
    rows.extend_from_slice(&third.to_be_bytes());
}

/// A byte position as the 4-byte offset field of the cross-reference.
fn field_offset(position: usize) -> Result<u32> {
    u32::try_from(position)
        .map_err(|_| Error::Other("file offset exceeds the 4-byte xref field".to_string()))
}

/// A `Name` from a string literal.
fn literal(text: &str) -> Name {
    Name(text.to_string())
}

#[cfg(test)]
mod tests {
    use pdfboss_core::xref::load_xref;
    use pdfboss_core::{Dict, Document, Name, ObjRef, Object, Stream};

    use super::*;
    use crate::error::Error;

    const CONTENT: &[u8] = b"BT /F1 12 Tf 72 720 Td (Hello, writer) Tj ET";

    fn name(text: &str) -> Name {
        Name(text.to_string())
    }

    fn table_options() -> WriteOptions {
        WriteOptions {
            xref: XrefStyle::Table,
            compress: false,
            object_streams: false,
            version: (1, 7),
        }
    }

    fn stream_options() -> WriteOptions {
        WriteOptions {
            xref: XrefStyle::Stream,
            compress: false,
            object_streams: false,
            version: (1, 5),
        }
    }

    fn objstm_options() -> WriteOptions {
        WriteOptions {
            xref: XrefStyle::Stream,
            compress: true,
            object_streams: true,
            version: (1, 7),
        }
    }

    struct Refs {
        content: ObjRef,
        pages: ObjRef,
        page: ObjRef,
        root: ObjRef,
    }

    fn page_dict(pages: ObjRef, content: ObjRef) -> Dict {
        let mut page = Dict::new();
        page.insert(name("Type"), Object::Name(name("Page")));
        page.insert(name("Parent"), Object::Ref(pages));
        page.insert(
            name("MediaBox"),
            Object::Array(vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(612),
                Object::Int(792),
            ]),
        );
        page.insert(name("Contents"), Object::Ref(content));
        page
    }

    /// Builds a one-page document around the given content stream, going
    /// through `put_stream_raw` when `raw` is set.
    fn skeleton(
        options: WriteOptions,
        content_dict: Dict,
        content_data: Vec<u8>,
        raw: bool,
    ) -> (Vec<u8>, Refs) {
        let mut w = Writer::new(options);
        let content = if raw {
            w.put_stream_raw(content_dict, content_data)
        } else {
            w.put_stream(content_dict, content_data)
        };
        let pages = w.reserve();
        let page = w.put(Object::Dict(page_dict(pages, content)));
        let mut tree = Dict::new();
        tree.insert(name("Type"), Object::Name(name("Pages")));
        tree.insert(name("Kids"), Object::Array(vec![Object::Ref(page)]));
        tree.insert(name("Count"), Object::Int(1));
        w.fill(pages, Object::Dict(tree))
            .expect("pages slot is fillable");
        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages));
        let root = w.put(Object::Dict(catalog));
        let bytes = w.finish(root).expect("minimal document finishes");
        (
            bytes,
            Refs {
                content,
                pages,
                page,
                root,
            },
        )
    }

    fn minimal_pdf(options: WriteOptions) -> (Vec<u8>, Refs) {
        skeleton(options, Dict::new(), CONTENT.to_vec(), false)
    }

    fn assert_minimal_loads(bytes: &[u8], refs: &Refs) -> Document {
        let doc = Document::load(bytes.to_vec()).expect("document loads");
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).expect("page 0 exists");
        assert_eq!(page.object_ref(), Some(refs.page));
        assert_eq!(page.dict(), &page_dict(refs.pages, refs.content));
        assert_eq!(page.content(&doc).expect("content decodes"), CONTENT);
        doc
    }

    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    #[test]
    fn table_mode_minimal_document_loads() {
        let (bytes, refs) = minimal_pdf(table_options());
        assert!(bytes.starts_with(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n"));
        assert!(bytes.ends_with(b"%%EOF"));
        assert_eq!(count_occurrences(&bytes, b"xref\n0 5\n"), 1);
        assert_eq!(count_occurrences(&bytes, b"0000000000 65535 f\r\n"), 1);
        assert_eq!(count_occurrences(&bytes, b"trailer\n"), 1);
        assert_minimal_loads(&bytes, &refs);
    }

    #[test]
    fn table_mode_ignores_object_streams_option() {
        let options = WriteOptions {
            object_streams: true,
            ..table_options()
        };
        let (bytes, refs) = minimal_pdf(options);
        assert_eq!(count_occurrences(&bytes, b"/ObjStm"), 0);
        assert_minimal_loads(&bytes, &refs);
    }

    #[test]
    fn stream_mode_minimal_document_loads() {
        let (bytes, refs) = minimal_pdf(stream_options());
        assert!(bytes.starts_with(b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n"));
        assert!(bytes.ends_with(b"%%EOF"));
        assert_eq!(count_occurrences(&bytes, b"/ObjStm"), 0);
        assert_eq!(count_occurrences(&bytes, b"/XRef"), 1);
        assert_minimal_loads(&bytes, &refs);
    }

    #[test]
    fn object_streams_pack_and_resolve() {
        let (bytes, refs) = minimal_pdf(objstm_options());
        assert_eq!(count_occurrences(&bytes, b"/ObjStm"), 1);
        let doc = assert_minimal_loads(&bytes, &refs);
        let root = doc
            .resolve(&Object::Ref(refs.root))
            .expect("catalog resolves");
        let catalog = root.as_dict().expect("catalog is a dictionary");
        assert_eq!(catalog.get_name("Type"), Some(&name("Catalog")));
        assert_eq!(catalog.get_ref("Pages"), Some(refs.pages));
    }

    #[test]
    fn object_streams_chunk_at_two_hundred() {
        let options = WriteOptions {
            compress: false,
            ..objstm_options()
        };
        let mut w = Writer::new(options);
        let content = w.put_stream(Dict::new(), CONTENT.to_vec());
        let int_refs: Vec<ObjRef> = (0..205).map(|i| w.put(Object::Int(i))).collect();
        let pages = w.reserve();
        let page = w.put(Object::Dict(page_dict(pages, content)));
        let mut tree = Dict::new();
        tree.insert(name("Type"), Object::Name(name("Pages")));
        tree.insert(name("Kids"), Object::Array(vec![Object::Ref(page)]));
        tree.insert(name("Count"), Object::Int(1));
        w.fill(pages, Object::Dict(tree))
            .expect("pages slot is fillable");
        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages));
        let root = w.put(Object::Dict(catalog));
        let bytes = w.finish(root).expect("document finishes");
        assert_eq!(count_occurrences(&bytes, b"/ObjStm"), 2);
        let doc = Document::load(bytes).expect("document loads");
        assert_eq!(
            doc.resolve(&Object::Ref(int_refs[0])).expect("resolves"),
            Object::Int(0)
        );
        assert_eq!(
            doc.resolve(&Object::Ref(int_refs[204])).expect("resolves"),
            Object::Int(204)
        );
        assert_eq!(doc.page_count(), 1);
    }

    #[test]
    fn refs_ascend_from_one_in_call_order() {
        let mut w = Writer::new(table_options());
        assert_eq!(w.put(Object::Null), ObjRef { num: 1, gen: 0 });
        assert_eq!(w.reserve(), ObjRef { num: 2, gen: 0 });
        assert_eq!(
            w.put_stream(Dict::new(), Vec::new()),
            ObjRef { num: 3, gen: 0 }
        );
        assert_eq!(
            w.put_stream_raw(Dict::new(), Vec::new()),
            ObjRef { num: 4, gen: 0 }
        );
    }

    #[test]
    fn fill_twice_reports_already_filled() {
        let mut w = Writer::new(table_options());
        let r = w.reserve();
        w.fill(r, Object::Int(1)).expect("first fill lands");
        match w.fill(r, Object::Int(2)) {
            Err(Error::AlreadyFilled(seen)) => assert_eq!(seen, r),
            other => panic!("expected AlreadyFilled, got {other:?}"),
        }
    }

    #[test]
    fn fill_rejects_foreign_and_wrong_generation_refs() {
        let mut w = Writer::new(table_options());
        let r = w.reserve();
        let unallocated = w.fill(ObjRef { num: 99, gen: 0 }, Object::Null);
        assert!(matches!(unallocated, Err(Error::Other(msg)) if msg.contains("99")));
        let zero = w.fill(ObjRef { num: 0, gen: 0 }, Object::Null);
        assert!(matches!(zero, Err(Error::Other(msg)) if !msg.is_empty()));
        let wrong_gen = w.fill(ObjRef { num: r.num, gen: 1 }, Object::Null);
        assert!(matches!(wrong_gen, Err(Error::Other(msg)) if msg.contains("generation")));
    }

    #[test]
    fn finish_with_unfilled_reserve_reports_the_ref() {
        let mut w = Writer::new(table_options());
        let root = w.put(Object::Dict(Dict::new()));
        let reserved = w.reserve();
        match w.finish(root) {
            Err(Error::Unfilled(seen)) => assert_eq!(seen, reserved),
            other => panic!("expected Unfilled, got {other:?}"),
        }
    }

    #[test]
    fn nested_stream_surfaces_from_finish() {
        for options in [table_options(), objstm_options()] {
            let mut w = Writer::new(options);
            let root = w.put(Object::Array(vec![Object::Stream(Stream {
                dict: Dict::new(),
                data: b"x".to_vec(),
            })]));
            assert!(matches!(w.finish(root), Err(Error::NestedStream)));
        }
    }

    #[test]
    fn compressed_stream_round_trips() {
        let options = WriteOptions {
            compress: true,
            ..table_options()
        };
        let data: Vec<u8> = b"q 0.5 0 0 0.5 36 36 cm Q\n".repeat(40);
        let (bytes, refs) = skeleton(options, Dict::new(), data.clone(), false);
        let doc = Document::load(bytes).expect("document loads");
        let resolved = doc
            .resolve(&Object::Ref(refs.content))
            .expect("content stream resolves");
        let stream = resolved.as_stream().expect("content is a stream");
        assert_eq!(stream.dict.get_name("Filter"), Some(&name("FlateDecode")));
        assert_eq!(
            stream.dict.get_int("Length"),
            Some(stream.data.len() as i64)
        );
        assert!(stream.data.len() < data.len());
        assert_eq!(doc.stream_data(stream).expect("stream decodes"), data);
    }

    #[test]
    fn preset_filter_is_not_recompressed() {
        let options = WriteOptions {
            compress: true,
            ..table_options()
        };
        let payload = b"Hello writer";
        let encoded: Vec<u8> = payload
            .iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .chain(*b">")
            .collect();
        let mut dict = Dict::new();
        dict.insert(name("Filter"), Object::Name(name("ASCIIHexDecode")));
        let (bytes, refs) = skeleton(options, dict, encoded.clone(), false);
        let doc = Document::load(bytes).expect("document loads");
        let resolved = doc
            .resolve(&Object::Ref(refs.content))
            .expect("content stream resolves");
        let stream = resolved.as_stream().expect("content is a stream");
        assert_eq!(stream.data, encoded, "pre-filtered data stays untouched");
        assert_eq!(
            stream.dict.get_name("Filter"),
            Some(&name("ASCIIHexDecode"))
        );
        assert_eq!(doc.stream_data(stream).expect("stream decodes"), payload);
    }

    #[test]
    fn put_stream_raw_never_compresses() {
        let options = WriteOptions {
            compress: true,
            ..table_options()
        };
        let (bytes, refs) = skeleton(options, Dict::new(), CONTENT.to_vec(), true);
        let doc = Document::load(bytes).expect("document loads");
        let resolved = doc
            .resolve(&Object::Ref(refs.content))
            .expect("content stream resolves");
        let stream = resolved.as_stream().expect("content is a stream");
        assert_eq!(stream.data, CONTENT);
        assert!(stream.dict.get("Filter").is_none());
        assert_eq!(stream.dict.get_int("Length"), Some(CONTENT.len() as i64));
    }

    #[test]
    fn id_pair_is_present_and_identical() {
        for options in [table_options(), stream_options(), objstm_options()] {
            let (bytes, refs) = minimal_pdf(options);
            assert_eq!(refs.root.gen, 0);
            let xref = load_xref(&bytes).expect("xref loads");
            let id = xref.trailer.get_array("ID").expect("/ID array present");
            assert_eq!(id.len(), 2);
            let first = id[0].as_str_bytes().expect("/ID entry is a string");
            let second = id[1].as_str_bytes().expect("/ID entry is a string");
            assert_eq!(first.len(), 16);
            assert_eq!(first, second);
        }
    }

    #[test]
    fn output_is_deterministic() {
        for options in [table_options(), stream_options(), objstm_options()] {
            let (first, refs) = minimal_pdf(options);
            let (second, again) = minimal_pdf(options);
            assert_eq!(refs.content, again.content);
            assert_eq!(first, second, "options {options:?} must be deterministic");
        }
    }
}
