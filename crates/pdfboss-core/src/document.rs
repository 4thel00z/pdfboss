//! The document model: loading, object resolution with caching, the
//! flattened page tree with attribute inheritance, and document metadata.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use crate::error::{Error, Result};
use crate::filters;
use crate::geom::Rect;
use crate::object::{decode_text_string, Dict, ObjRef, Object, Stream};
use crate::objstm;
use crate::parser::{Parser, Resolve};
use crate::xref::{load_xref, Xref, XrefEntry};

/// Default page size when `/MediaBox` is absent or invalid: US Letter.
const US_LETTER: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);

/// Reference-chase depth limit for [`Document::resolve`].
const MAX_RESOLVE_DEPTH: usize = 32;

/// Page-tree traversal depth cap.
const MAX_TREE_DEPTH: usize = 256;

/// A loaded PDF document.
pub struct Document {
    data: Vec<u8>,
    version: (u8, u8),
    xref: Xref,
    /// Interior cache of fetched indirect objects.
    cache: RefCell<HashMap<(u32, u16), Rc<Object>>>,
    /// Object numbers currently being parsed, guarding re-entrant fetches
    /// (e.g. a stream whose `/Length` refers back to the stream itself).
    loading: RefCell<HashSet<u32>>,
    /// Decoded object streams, keyed by their stream object number, so a
    /// stream is decompressed and its header parsed at most once even when
    /// many compressed objects are read from it.
    objstms: RefCell<HashMap<u32, Rc<objstm::ObjStm>>>,
    pages: Vec<PageRec>,
}

/// The flattened, inheritance-applied record for one page.
struct PageRec {
    media_box: Rect,
    crop_box: Rect,
    rotate: i32,
    resources: Dict,
    dict: Dict,
}

/// Attributes inherited down the page tree (ISO 32000 §7.7.3.4).
#[derive(Clone, Default)]
struct Inherited {
    resources: Option<Dict>,
    media_box: Option<Rect>,
    crop_box: Option<Rect>,
    rotate: Option<i32>,
}

/// Parses the `%PDF-x.y` header, scanning the first 1 KiB; absent or
/// malformed headers default to version 1.4.
fn parse_version(data: &[u8]) -> (u8, u8) {
    try_parse_version(data).unwrap_or((1, 4))
}

fn try_parse_version(data: &[u8]) -> Option<(u8, u8)> {
    let window = &data[..data.len().min(1024)];
    let pos = memchr::memmem::find(window, b"%PDF-")?;
    let rest = &window[pos + 5..];
    let (major, used) = read_version_component(rest)?;
    if rest.get(used) != Some(&b'.') {
        return None;
    }
    let (minor, _) = read_version_component(&rest[used + 1..])?;
    Some((major, minor))
}

/// Reads a run of 1–3 ASCII digits as a `u8`, returning the value and the
/// number of bytes consumed.
fn read_version_component(bytes: &[u8]) -> Option<(u8, usize)> {
    let end = bytes
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(bytes.len());
    if end == 0 || end > 3 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..end]).ok()?.parse().ok()?;
    Some((value, end))
}

/// Normalizes a `/Rotate` value to one of {0, 90, 180, 270}; values that
/// are not multiples of 90 fall back to 0 (lenient).
fn normalize_rotation(deg: i32) -> i32 {
    let r = deg.rem_euclid(360);
    if r % 90 == 0 {
        r
    } else {
        0
    }
}

impl Document {
    /// Loads a document from bytes: locates the `%PDF-x.y` header (scanning
    /// the first 1 KiB, defaulting to 1.4), loads the xref, rejects
    /// encrypted files with [`Error::Encrypted`], and flattens the page
    /// tree.
    pub fn load(data: Vec<u8>) -> Result<Document> {
        let version = parse_version(&data);
        let xref = load_xref(&data)?;
        if xref.trailer.get("Encrypt").is_some_and(|o| !o.is_null()) {
            return Err(Error::Encrypted);
        }
        let mut doc = Document {
            data,
            version,
            xref,
            cache: RefCell::new(HashMap::new()),
            loading: RefCell::new(HashSet::new()),
            objstms: RefCell::new(HashMap::new()),
            pages: Vec::new(),
        };
        doc.pages = doc.flatten_pages();
        Ok(doc)
    }

    /// Reads the file at `path` and loads it via [`Document::load`].
    pub fn open(path: impl AsRef<Path>) -> Result<Document> {
        Document::load(std::fs::read(path)?)
    }

    /// The PDF version from the header, e.g. `(1, 7)`.
    pub fn version(&self) -> (u8, u8) {
        self.version
    }

    /// Fetches an indirect object by reference (xref lookup, object-stream
    /// indirection, cached). A generation mismatch between the request and
    /// the file is tolerated (lenient).
    pub fn get(&self, r: ObjRef) -> Result<Object> {
        if let Some(cached) = self.cache.borrow().get(&(r.num, r.gen)) {
            return Ok((**cached).clone());
        }
        if !self.loading.borrow_mut().insert(r.num) {
            return Err(Error::CircularReference(r.num));
        }
        let result = self.load_object(r);
        self.loading.borrow_mut().remove(&r.num);
        let object = result?;
        self.cache
            .borrow_mut()
            .insert((r.num, r.gen), Rc::new(object.clone()));
        Ok(object)
    }

    /// Uncached fetch: parses the object at its file offset or extracts it
    /// from its containing object stream.
    fn load_object(&self, r: ObjRef) -> Result<Object> {
        match self.xref.get(r.num) {
            None | Some(XrefEntry::Free) => Err(Error::ObjectNotFound(r.num, r.gen)),
            Some(XrefEntry::InFile { offset, .. }) => {
                let offset = usize::try_from(offset)
                    .ok()
                    .filter(|&o| o < self.data.len())
                    .ok_or(Error::ObjectNotFound(r.num, r.gen))?;
                let (_, object) = Parser::at(&self.data, offset).parse_indirect(self)?;
                Ok(object)
            }
            Some(XrefEntry::InStream { stream_num, index }) => {
                self.load_from_object_stream(stream_num, index)
            }
        }
    }

    /// Extracts a compressed object from the object stream `stream_num`,
    /// decoding and parsing that stream's header at most once.
    fn load_from_object_stream(&self, stream_num: u32, index: u32) -> Result<Object> {
        if let Some(stm) = self.objstms.borrow().get(&stream_num) {
            return stm.object(index);
        }
        let container = self.get(ObjRef {
            num: stream_num,
            gen: 0,
        })?;
        let stream = container.as_stream().ok_or_else(|| Error::TypeMismatch {
            expected: "stream",
            found: type_name(&container),
        })?;
        let n = self
            .resolve(stream.dict.get("N").unwrap_or(&Object::Null))?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::MissingKey("N"))?;
        let first = self
            .resolve(stream.dict.get("First").unwrap_or(&Object::Null))?
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::MissingKey("First"))?;
        let decoded = self.stream_data(stream)?;
        let stm = Rc::new(objstm::ObjStm::parse(decoded, n, first)?);
        let object = stm.object(index)?;
        self.objstms.borrow_mut().insert(stream_num, stm);
        Ok(object)
    }

    /// Chases reference chains with a depth guard of `MAX_RESOLVE_DEPTH`
    /// (beyond that: [`Error::CircularReference`]); a reference to a missing
    /// or unreadable object resolves to `Null` (lenient).
    pub fn resolve(&self, o: &Object) -> Result<Object> {
        let mut current = o.clone();
        let mut last_num = 0;
        for _ in 0..MAX_RESOLVE_DEPTH {
            match current {
                Object::Ref(r) => {
                    last_num = r.num;
                    current = match self.get(r) {
                        Ok(object) => object,
                        Err(Error::CircularReference(n)) => {
                            return Err(Error::CircularReference(n))
                        }
                        Err(_) => return Ok(Object::Null),
                    };
                }
                other => return Ok(other),
            }
        }
        Err(Error::CircularReference(last_num))
    }

    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this document.
    pub fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
        filters::decode_stream(s, self)
    }

    /// Number of pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The page at 0-based `index`.
    pub fn page(&self, index: usize) -> Result<Page> {
        let rec = self
            .pages
            .get(index)
            .ok_or(Error::PageNotFound(index, self.pages.len()))?;
        Ok(Page {
            index,
            media_box: rec.media_box,
            crop_box: rec.crop_box,
            rotate: rec.rotate,
            resources: rec.resources.clone(),
            dict: rec.dict.clone(),
        })
    }

    /// Document metadata from the trailer `/Info` dictionary (lenient:
    /// absent or malformed entries are simply `None`).
    pub fn metadata(&self) -> Metadata {
        let mut meta = Metadata::default();
        let Some(info) = self.xref.trailer.get("Info") else {
            return meta;
        };
        let Ok(info) = self.resolve(info) else {
            return meta;
        };
        let Some(dict) = info.as_dict() else {
            return meta;
        };
        meta.title = self.meta_string(dict, "Title");
        meta.author = self.meta_string(dict, "Author");
        meta.subject = self.meta_string(dict, "Subject");
        meta.keywords = self.meta_string(dict, "Keywords");
        meta.creator = self.meta_string(dict, "Creator");
        meta.producer = self.meta_string(dict, "Producer");
        meta.creation_date = self.meta_string(dict, "CreationDate");
        meta.mod_date = self.meta_string(dict, "ModDate");
        meta
    }

    /// Reads `key` from an info dictionary as a decoded text string.
    fn meta_string(&self, dict: &Dict, key: &str) -> Option<String> {
        let value = self.resolve(dict.get(key)?).ok()?;
        Some(decode_text_string(value.as_str_bytes()?))
    }

    /// Flattens the page tree by iterative depth-first traversal of `/Kids`
    /// with a visited-reference cycle guard and a depth cap, applying
    /// attribute inheritance. Any structural problem simply truncates or
    /// skips (lenient) — this never fails.
    fn flatten_pages(&self) -> Vec<PageRec> {
        let mut pages = Vec::new();
        let Some(root) = self.xref.trailer.get("Root") else {
            return pages;
        };
        let Ok(catalog) = self.resolve(root) else {
            return pages;
        };
        let Some(tree_root) = catalog.as_dict().and_then(|d| d.get("Pages")) else {
            return pages;
        };
        let mut visited: HashSet<ObjRef> = HashSet::new();
        let mut stack: Vec<(Object, Inherited, usize)> =
            vec![(tree_root.clone(), Inherited::default(), 0)];
        while let Some((node, mut inherited, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH {
                continue;
            }
            if let Object::Ref(r) = node {
                if !visited.insert(r) {
                    continue; // cycle: this node was already traversed
                }
            }
            let Ok(resolved) = self.resolve(&node) else {
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            if let Some(res) = self.dict_value(dict, "Resources") {
                inherited.resources = Some(res);
            }
            if let Some(mb) = self.rect_value(dict, "MediaBox") {
                inherited.media_box = Some(mb);
            }
            if let Some(cb) = self.rect_value(dict, "CropBox") {
                inherited.crop_box = Some(cb);
            }
            if let Some(rot) = self.int_value(dict, "Rotate") {
                inherited.rotate = Some(rot);
            }
            let is_page = dict.get_name("Type").is_some_and(|n| n.0 == "Page");
            let kids = if is_page {
                None
            } else {
                self.array_value(dict, "Kids")
            };
            match kids {
                Some(kids) => {
                    // Reverse push so pop order matches document order.
                    for kid in kids.iter().rev() {
                        stack.push((kid.clone(), inherited.clone(), depth + 1));
                    }
                }
                None => pages.push(make_page_rec(dict.clone(), &inherited)),
            }
        }
        pages
    }

    /// Resolves `dict[key]` to a dictionary, if present and well-formed.
    fn dict_value(&self, dict: &Dict, key: &str) -> Option<Dict> {
        self.resolve(dict.get(key)?).ok()?.as_dict().cloned()
    }

    /// Resolves `dict[key]` to an array, if present and well-formed.
    fn array_value(&self, dict: &Dict, key: &str) -> Option<Vec<Object>> {
        match self.resolve(dict.get(key)?).ok()? {
            Object::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Resolves `dict[key]` to an integer (reals truncate, lenient).
    fn int_value(&self, dict: &Dict, key: &str) -> Option<i32> {
        let v = self.resolve(dict.get(key)?).ok()?.as_f64()?;
        if v.is_finite() {
            Some(v as i32)
        } else {
            None
        }
    }

    /// Resolves `dict[key]` to a normalized rectangle: a four-number array
    /// whose elements may themselves be references.
    fn rect_value(&self, dict: &Dict, key: &str) -> Option<Rect> {
        let items = self.array_value(dict, key)?;
        if items.len() != 4 {
            return None;
        }
        let mut coords = [0.0f32; 4];
        for (slot, item) in coords.iter_mut().zip(&items) {
            let n = self.resolve(item).ok()?.as_f64()?;
            if !n.is_finite() {
                return None;
            }
            *slot = n as f32;
        }
        Some(Rect::new(coords[0], coords[1], coords[2], coords[3]).normalize())
    }
}

/// Builds the final page record from a leaf dictionary and its inherited
/// attributes, applying the spec defaults.
fn make_page_rec(dict: Dict, inherited: &Inherited) -> PageRec {
    let media_box = inherited
        .media_box
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(US_LETTER);
    let crop_box = inherited
        .crop_box
        .and_then(|c| c.intersect(media_box))
        .filter(|r| r.width() > 0.0 && r.height() > 0.0)
        .unwrap_or(media_box);
    PageRec {
        media_box,
        crop_box,
        rotate: normalize_rotation(inherited.rotate.unwrap_or(0)),
        resources: inherited.resources.clone().unwrap_or_default(),
        dict,
    }
}

/// Human-readable object type name for error messages.
fn type_name(o: &Object) -> &'static str {
    match o {
        Object::Null => "null",
        Object::Bool(_) => "boolean",
        Object::Int(_) => "integer",
        Object::Real(_) => "real",
        Object::String(_) => "string",
        Object::Name(_) => "name",
        Object::Array(_) => "array",
        Object::Dict(_) => "dictionary",
        Object::Stream(_) => "stream",
        Object::Ref(_) => "reference",
    }
}

impl Resolve for Document {
    fn resolve_ref(&self, r: ObjRef) -> Option<Object> {
        self.get(r).ok()
    }
}

/// Document information from the trailer `/Info` dictionary. Only present,
/// well-formed entries are populated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub mod_date: Option<String>,
}

/// A single page with inherited attributes already applied.
///
/// Defaults: `media_box` falls back to US Letter (612x792) when absent or
/// invalid, `crop_box` falls back to (and is intersected with) `media_box`,
/// and `rotate` is normalized to one of {0, 90, 180, 270}.
pub struct Page {
    /// 0-based page index.
    pub index: usize,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub rotate: i32,
    /// The page's (inherited) `/Resources` dictionary.
    pub resources: Dict,
    dict: Dict,
}

impl Page {
    /// The page's decoded content: the `/Contents` stream, or all streams
    /// of a `/Contents` array decoded and joined with `b"\n"`. A missing
    /// `/Contents` yields empty content (lenient).
    pub fn content(&self, doc: &Document) -> Result<Vec<u8>> {
        let Some(contents) = self.dict.get("Contents") else {
            return Ok(Vec::new());
        };
        match doc.resolve(contents)? {
            Object::Stream(ref s) => doc.stream_data(s),
            Object::Array(items) => {
                let mut out = Vec::new();
                let mut first = true;
                for item in &items {
                    let part = doc.resolve(item)?;
                    let Some(stream) = part.as_stream() else {
                        continue; // non-stream entries are skipped (lenient)
                    };
                    if !first {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(&doc.stream_data(stream)?);
                    first = false;
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Crop-box width and height, swapped when `/Rotate` is 90 or 270.
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        if self.rotate == 90 || self.rotate == 270 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// The raw page dictionary.
    pub fn dict(&self) -> &Dict {
        &self.dict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_testkit::{multi_page_doc, objstm_payload, simple_doc, PdfBuilder};

    /// Replaces the first occurrence of `from` with `to`. Splicing happens
    /// after the xref section, so byte offsets stay valid.
    fn replace_once(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let pos = memchr::memmem::find(data, from).expect("pattern present in fixture");
        let mut out = Vec::with_capacity(data.len() - from.len() + to.len());
        out.extend_from_slice(&data[..pos]);
        out.extend_from_slice(to);
        out.extend_from_slice(&data[pos + from.len()..]);
        out
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        memchr::memmem::find(haystack, needle).is_some()
    }

    const FONT: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

    #[test]
    fn loads_simple_doc() {
        let doc = Document::load(simple_doc("Greetings, cosmos!")).unwrap();
        assert_eq!(doc.version(), (1, 7));
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).unwrap();
        assert_eq!(page.index, 0);
        assert_eq!(page.media_box, Rect::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(page.crop_box, page.media_box);
        assert_eq!(page.rotate, 0);
        assert_eq!(page.size(), (612.0, 792.0));
        assert!(page.resources.get("Font").is_some());
        let content = page.content(&doc).unwrap();
        assert!(contains(&content, b"Greetings, cosmos!"));
    }

    #[test]
    fn multi_page_ordering() {
        let doc = Document::load(multi_page_doc(&["alpha", "beta", "gamma"])).unwrap();
        assert_eq!(doc.page_count(), 3);
        for (i, text) in ["alpha", "beta", "gamma"].iter().enumerate() {
            let content = doc.page(i).unwrap().content(&doc).unwrap();
            assert!(
                contains(&content, text.as_bytes()),
                "page {i} should show {text}"
            );
        }
    }

    #[test]
    fn page_index_out_of_bounds() {
        let doc = Document::load(simple_doc("x")).unwrap();
        assert!(matches!(doc.page(5), Err(Error::PageNotFound(5, 1))));
    }

    #[test]
    fn open_reads_from_disk() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("pdfboss-doc-test-{}.pdf", std::process::id()));
        std::fs::write(&path, simple_doc("from disk")).unwrap();
        let doc = Document::open(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(doc.page_count(), 1);
        let content = doc.page(0).unwrap().content(&doc).unwrap();
        assert!(contains(&content, b"from disk"));
        assert!(matches!(
            Document::open(dir.join("pdfboss-doc-test-missing.pdf")),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn encrypt_in_trailer_is_rejected() {
        let data = replace_once(
            &simple_doc("secret"),
            b"trailer\n<< /Size",
            b"trailer\n<< /Encrypt 9 0 R /Size",
        );
        assert!(matches!(Document::load(data), Err(Error::Encrypted)));
    }

    #[test]
    fn metadata_utf16be_round_trip() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        // /Title is UTF-16BE with BOM: "H\u{151}" (H + o with double acute).
        b.object(6, "<< /Title <FEFF00480151> /Author (plain author) >>");
        let data = replace_once(&b.build(1), b"<< /Size", b"<< /Info 6 0 R /Size");
        let doc = Document::load(data).unwrap();
        let meta = doc.metadata();
        assert_eq!(meta.title.as_deref(), Some("H\u{151}"));
        assert_eq!(meta.author.as_deref(), Some("plain author"));
        assert_eq!(meta.subject, None);
        assert_eq!(meta.keywords, None);
        assert_eq!(meta.creation_date, None);
    }

    #[test]
    fn metadata_without_info_is_all_none() {
        let doc = Document::load(simple_doc("x")).unwrap();
        assert_eq!(doc.metadata(), Metadata::default());
    }

    #[test]
    fn missing_object_resolves_to_null() {
        let doc = Document::load(simple_doc("x")).unwrap();
        let missing = Object::Ref(ObjRef { num: 99, gen: 0 });
        assert_eq!(doc.resolve(&missing).unwrap(), Object::Null);
        assert!(matches!(
            doc.get(ObjRef { num: 99, gen: 0 }),
            Err(Error::ObjectNotFound(99, 0))
        ));
    }

    #[test]
    fn self_reference_is_circular() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog >>");
        b.object(6, "6 0 R");
        let doc = Document::load(b.build(1)).unwrap();
        let loops = Object::Ref(ObjRef { num: 6, gen: 0 });
        assert!(matches!(
            doc.resolve(&loops),
            Err(Error::CircularReference(6))
        ));
    }

    #[test]
    fn generation_mismatch_is_tolerated() {
        let doc = Document::load(simple_doc("x")).unwrap();
        let catalog = doc.get(ObjRef { num: 1, gen: 7 }).unwrap();
        let dict = catalog.as_dict().unwrap();
        assert_eq!(dict.get_name("Type").map(|n| n.0.as_str()), Some("Catalog"));
    }

    #[test]
    fn objects_in_object_streams_are_fetched() {
        let mut b = PdfBuilder::new();
        let (dict, payload) =
            objstm_payload(&[(1, "<< /Type /Catalog /Pages 2 0 R >>"), (5, FONT)]);
        b.stream(6, &dict, &payload);
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf (compressed hello) Tj ET");
        let doc = Document::load(b.build_xref_stream(1)).unwrap();
        assert_eq!(doc.page_count(), 1);
        let page = doc.page(0).unwrap();
        assert!(contains(&page.content(&doc).unwrap(), b"compressed hello"));
        let font = doc.get(ObjRef { num: 5, gen: 0 }).unwrap();
        assert_eq!(
            font.as_dict()
                .and_then(|d| d.get_name("BaseFont"))
                .map(|n| n.0.as_str()),
            Some("Helvetica")
        );
    }

    #[test]
    fn contents_array_is_joined_with_newlines() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents [4 0 R null 5 0 R] >>",
        );
        b.stream(4, "", b"q");
        b.stream(5, "", b"Q");
        let doc = Document::load(b.build(1)).unwrap();
        let content = doc.page(0).unwrap().content(&doc).unwrap();
        assert_eq!(content, b"q\nQ", "streams joined by \\n, null skipped");
    }

    #[test]
    fn inheritance_from_pages_node_and_rotate_swap() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 \
             /Resources << /Font << /F1 5 0 R >> >> /MediaBox [0 0 400 600] >>",
        );
        b.object(3, "<< /Type /Page /Parent 2 0 R >>");
        b.object(4, "<< /Type /Page /Parent 2 0 R /Rotate 270 >>");
        b.object(5, FONT);
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(doc.page_count(), 2);

        let first = doc.page(0).unwrap();
        assert_eq!(first.media_box, Rect::new(0.0, 0.0, 400.0, 600.0));
        assert_eq!(first.crop_box, first.media_box);
        assert!(first.resources.get("Font").is_some(), "inherited resources");
        assert_eq!(first.rotate, 0);
        assert!(
            first.content(&doc).unwrap().is_empty(),
            "no /Contents means empty content"
        );
        assert_eq!(first.size(), (400.0, 600.0));

        let second = doc.page(1).unwrap();
        assert_eq!(second.rotate, 270);
        assert_eq!(second.size(), (600.0, 400.0), "rotate 270 swaps w/h");
    }

    #[test]
    fn crop_box_intersected_and_rotate_normalized() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /CropBox [100 100 400 400] /Rotate 450 >>",
        );
        b.object(4, "<< /Type /Page /Parent 2 0 R /Rotate -90 >>");
        b.object(
            5,
            "<< /Type /Page /Parent 2 0 R /Rotate 45 /MediaBox [0 0 0 0] >>",
        );
        let doc = Document::load(b.build(1)).unwrap();

        let clipped = doc.page(0).unwrap();
        assert_eq!(clipped.crop_box, Rect::new(100.0, 100.0, 200.0, 200.0));
        assert_eq!(clipped.rotate, 90, "450 normalizes to 90");
        assert_eq!(clipped.size(), (100.0, 100.0));

        assert_eq!(doc.page(1).unwrap().rotate, 270, "-90 normalizes to 270");
        let odd = doc.page(2).unwrap();
        assert_eq!(odd.rotate, 0, "non-multiple of 90 falls back to 0");
        assert_eq!(odd.media_box, US_LETTER, "degenerate media box defaults");
    }

    #[test]
    fn kids_cycle_truncates_without_hanging() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        // 2 → 3 → {4, back to 2}: the back-edge must be ignored.
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Pages /Kids [4 0 R 2 0 R] /Count 1 >>");
        b.object(
            4,
            "<< /Type /Page /Parent 3 0 R /MediaBox [0 0 100 100] /Contents 5 0 R >>",
        );
        b.stream(5, "", b"0 0 50 50 re f");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(doc.page_count(), 1, "cycle back-edge yields no extra pages");
        assert!(contains(
            &doc.page(0).unwrap().content(&doc).unwrap(),
            b"re f"
        ));
    }

    #[test]
    fn tree_depth_is_capped() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        // A unary chain of 300 intermediate nodes, page leaf at the bottom.
        let last = 302u32;
        for num in 2..last {
            b.object(
                num,
                &format!("<< /Type /Pages /Kids [{} 0 R] /Count 1 >>", num + 1),
            );
        }
        b.object(last, "<< /Type /Page >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(doc.page_count(), 0, "leaf beyond the depth cap is dropped");
    }

    #[test]
    fn version_scan_and_default() {
        let mut b = PdfBuilder::new().version(2, 0);
        b.object(1, "<< /Type /Catalog >>");
        assert_eq!(Document::load(b.build(1)).unwrap().version(), (2, 0));
        // Corrupting the header magic (same length) falls back to 1.4.
        let data = replace_once(&simple_doc("v"), b"%PDF-", b"%QQQ-");
        assert_eq!(Document::load(data).unwrap().version(), (1, 4));
    }

    #[test]
    fn deeply_nested_root_object_does_not_overflow_the_stack() {
        // A ~100 KB file whose Root is a 50k-deep array used to drive the
        // object parser's recursion into a fatal stack overflow during
        // `Document::load`. Run on a small stack so a regression aborts
        // loudly rather than depending on the main thread's stack size.
        let mut data = b"%PDF-1.7\n1 0 obj\n".to_vec();
        data.extend(std::iter::repeat_n(b'[', 50_000));
        data.extend(std::iter::repeat_n(b']', 50_000));
        data.extend_from_slice(b"\nendobj\ntrailer\n<</Root 1 0 R>>\n%%EOF\n");
        let outcome = std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || Document::load(data).map(|doc| doc.page_count()))
            .expect("spawn test thread")
            .join()
            .expect("Document::load must not overflow the stack");
        // The over-nested Root is rejected or ignored (lenient), but the
        // process survives and no page is fabricated from it.
        assert!(matches!(outcome, Ok(0) | Err(_)));
    }
}
