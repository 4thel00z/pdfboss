//! The document model: loading, object resolution with caching, the
//! lazily-flattened page tree with attribute inheritance, and document
//! metadata.

use crate::hash::{FastMap, FastSet};
use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::rc::Rc;

use crate::crypt::Decryptor;
use crate::elements::Span;
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
    cache: RefCell<FastMap<(u32, u16), Rc<Object>>>,
    /// Object numbers currently being parsed, guarding re-entrant fetches
    /// (e.g. a stream whose `/Length` refers back to the stream itself).
    loading: RefCell<FastSet<u32>>,
    /// Decoded object streams, keyed by their stream object number, so a
    /// stream is decompressed and its header parsed at most once even when
    /// many compressed objects are read from it.
    objstms: RefCell<FastMap<u32, Rc<objstm::ObjStm>>>,
    /// Present when the file uses the Standard security handler (RC4 or AES)
    /// and opens under the empty user password; decrypts strings and stream
    /// data as objects are loaded from the file.
    decryptor: Option<Decryptor>,
    /// The flattened page tree, built lazily on the first page access so that
    /// merely opening a document (or reading its page count) never parses
    /// every page dictionary. See [`Document::pages`].
    pages: OnceCell<Vec<PageRec>>,
}

/// The flattened, inheritance-applied record for one page.
struct PageRec {
    obj_ref: Option<ObjRef>,
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
    /// the first 1 KiB, defaulting to 1.4), loads the xref, and sets up
    /// decryption for files using the Standard security handler (RC4 or AES)
    /// under the empty user password (password-protected files yield
    /// [`Error::Encrypted`]).
    ///
    /// The page tree is **not** walked here: it is flattened lazily on the
    /// first page access, so opening a document (or reading `page_count`) does
    /// not parse every page dictionary.
    pub fn load(data: Vec<u8>) -> Result<Document> {
        let version = parse_version(&data);
        let xref = load_xref(&data)?;
        let mut doc = Document {
            data,
            version,
            xref,
            cache: RefCell::new(FastMap::default()),
            loading: RefCell::new(FastSet::default()),
            objstms: RefCell::new(FastMap::default()),
            decryptor: None,
            pages: OnceCell::new(),
        };
        if doc
            .xref
            .trailer
            .get("Encrypt")
            .is_some_and(|o| !o.is_null())
        {
            doc.setup_decryption()?;
        }
        Ok(doc)
    }

    /// Configures decryption for an encrypted file. Supports the Standard
    /// security handler with RC4 (`/V` 1–2), AESV2 (`/V` 4) and AESV3 (`/V` 5)
    /// under the empty user password; a required password is reported as
    /// [`Error::Encrypted`]. Must run before any content object is fetched, and
    /// reads `/Encrypt` and `/ID` while decryption is still off (those values
    /// are stored unencrypted).
    fn setup_decryption(&mut self) -> Result<()> {
        let enc_obj = self
            .xref
            .trailer
            .get("Encrypt")
            .cloned()
            .unwrap_or(Object::Null);
        let enc = self.resolve(&enc_obj)?;
        let enc_dict = enc.as_dict().ok_or(Error::Encrypted)?;
        let id0: Vec<u8> = self
            .xref
            .trailer
            .get("ID")
            .and_then(Object::as_array)
            .and_then(<[Object]>::first)
            .and_then(Object::as_str_bytes)
            .unwrap_or(&[])
            .to_vec();
        match Decryptor::from_standard(enc_dict, &id0) {
            Some(dec) => {
                self.decryptor = Some(dec);
                // Objects fetched while resolving /Encrypt were cached without
                // decryption; drop them so they are re-read through the
                // decrypting path if referenced again.
                self.cache.borrow_mut().clear();
                Ok(())
            }
            None => Err(Error::Encrypted),
        }
    }

    /// Reads the file at `path` and loads it via [`Document::load`].
    pub fn open(path: impl AsRef<Path>) -> Result<Document> {
        Document::load(std::fs::read(path)?)
    }

    /// The PDF version from the header, e.g. `(1, 7)`.
    pub fn version(&self) -> (u8, u8) {
        self.version
    }

    /// Raw bytes of the loaded file.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// The merged cross-reference table and trailer.
    pub fn xref(&self) -> &Xref {
        &self.xref
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
                self.object_at_spanned(offset).map(|parsed| parsed.1)
            }
            Some(XrefEntry::InStream { stream_num, index }) => {
                self.load_from_object_stream(stream_num, index)
            }
        }
    }

    /// Parses the indirect object at `offset`, applying decryption, and
    /// reports the byte range consumed (`N G obj … endobj`).
    pub(crate) fn object_at_spanned(&self, offset: usize) -> Result<(ObjRef, Object, Span)> {
        let mut parser = Parser::at(&self.data, offset);
        let (r, mut object) = parser.parse_indirect(self)?;
        // Objects stored directly in the file carry encrypted strings and
        // stream data; decrypt with this object's key. (Objects living in
        // object streams are decrypted with their container.)
        if let Some(dec) = &self.decryptor {
            dec.decrypt_object(&mut object, r.num, r.gen);
        }
        Ok((r, object, Span::new(offset as u64, parser.pos() as u64)))
    }

    /// The decoded, header-parsed object stream `stream_num`, built at most
    /// once and cached.
    pub(crate) fn objstm_handle(&self, stream_num: u32) -> Result<Rc<objstm::ObjStm>> {
        if let Some(stm) = self.objstms.borrow().get(&stream_num) {
            return Ok(Rc::clone(stm));
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
        self.objstms
            .borrow_mut()
            .insert(stream_num, Rc::clone(&stm));
        Ok(stm)
    }

    /// Extracts a compressed object from the object stream `stream_num`.
    fn load_from_object_stream(&self, stream_num: u32, index: u32) -> Result<Object> {
        self.objstm_handle(stream_num)?.object(index)
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

    /// The flattened page tree, built once on first access.
    fn pages(&self) -> &[PageRec] {
        self.pages.get_or_init(|| self.flatten_pages())
    }

    /// Number of pages.
    ///
    /// Reports the page tree's declared `/Count` — the same value mature
    /// engines return — without walking the tree, so it is cheap on an
    /// otherwise-untouched document. If `/Count` is absent or implausible the
    /// tree is flattened and its true (lenient, cycle- and depth-guarded)
    /// length is returned instead. Once the tree has been flattened for any
    /// reason, that flattened length is authoritative.
    pub fn page_count(&self) -> usize {
        if let Some(pages) = self.pages.get() {
            return pages.len();
        }
        if let Some(count) = self.declared_page_count() {
            return count;
        }
        self.pages().len()
    }

    /// Reads the page tree root's `/Count` cheaply (Root → `/Pages` →
    /// `/Count`) without descending into `/Kids`. Returns `None` when the
    /// entry is missing, non-integer, negative, or larger than the file could
    /// possibly hold (a corrupt count), so the caller falls back to a real
    /// walk.
    fn declared_page_count(&self) -> Option<usize> {
        let root = self.xref.trailer.get("Root")?;
        let catalog = self.resolve(root).ok()?;
        let pages = self.resolve(catalog.as_dict()?.get("Pages")?).ok()?;
        let count = usize::try_from(self.int_value(pages.as_dict()?, "Count")?).ok()?;
        // A page occupies at least a handful of bytes on disk, so a count that
        // exceeds the file length is corrupt: fall back to walking the tree.
        (count <= self.data.len()).then_some(count)
    }

    /// The page at 0-based `index`.
    pub fn page(&self, index: usize) -> Result<Page> {
        let pages = self.pages();
        let rec = pages
            .get(index)
            .ok_or(Error::PageNotFound(index, pages.len()))?;
        Ok(Page {
            index,
            media_box: rec.media_box,
            crop_box: rec.crop_box,
            rotate: rec.rotate,
            resources: rec.resources.clone(),
            dict: rec.dict.clone(),
            obj_ref: rec.obj_ref,
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
        let mut visited: FastSet<ObjRef> = FastSet::default();
        let mut stack: Vec<(Object, Inherited, usize)> =
            vec![(tree_root.clone(), Inherited::default(), 0)];
        while let Some((node, mut inherited, depth)) = stack.pop() {
            if depth > MAX_TREE_DEPTH {
                continue;
            }
            let node_ref = if let Object::Ref(r) = node {
                Some(r)
            } else {
                None
            };
            if let Some(r) = node_ref {
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
                None => pages.push(make_page_rec(node_ref, dict.clone(), &inherited)),
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
fn make_page_rec(obj_ref: Option<ObjRef>, dict: Dict, inherited: &Inherited) -> PageRec {
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
        obj_ref,
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
    obj_ref: Option<ObjRef>,
}

impl Page {
    /// The page's indirect object reference, when the page came from an
    /// indirect kid in the page tree (pages inlined directly into a `/Kids`
    /// array have none).
    pub fn object_ref(&self) -> Option<ObjRef> {
        self.obj_ref
    }

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
    use crate::parser::{NoResolve, Parser};
    use crate::xref::XrefEntry;
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
        // `page_count` reports the tree's declared `/Count` (1) cheaply, as
        // mature engines do; the leaf itself lies beyond the traversal depth
        // cap, so the flattened tree is empty and the page cannot be
        // materialized.
        assert_eq!(doc.page_count(), 1, "declared /Count is reported cheaply");
        assert!(
            matches!(doc.page(0), Err(Error::PageNotFound(0, 0))),
            "leaf beyond the depth cap cannot be materialized"
        );
    }

    #[test]
    fn page_count_reports_declared_count_cheaply() {
        // The tree declares five pages but supplies only one kid. `page_count`
        // reports the declared `/Count` (as mature engines do) without walking,
        // while page access is bounded by the pages that actually materialize.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 5 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(doc.page_count(), 5, "declared /Count reported verbatim");
        assert!(doc.page(0).is_ok(), "the one real page materializes");
        assert!(
            matches!(doc.page(1), Err(Error::PageNotFound(1, 1))),
            "access past the real pages fails with the true length"
        );
    }

    #[test]
    fn page_count_falls_back_to_walk_when_count_absent() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] >>"); // no /Count
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(
            doc.page_count(),
            1,
            "missing /Count is recovered by walking"
        );
    }

    #[test]
    fn page_count_ignores_corrupt_oversized_count() {
        // A `/Count` larger than the whole file is impossible: fall back to a
        // real walk rather than trust it.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 999999999 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(doc.page_count(), 1, "implausible /Count is rejected");
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

    #[test]
    fn bytes_and_xref_accessors() {
        let data = simple_doc("accessors");
        let doc = Document::load(data.clone()).unwrap();
        assert_eq!(doc.bytes(), &data[..]);
        assert!(!doc.xref().is_empty());
        assert!(doc.xref().trailer.get("Root").is_some());
    }

    #[test]
    fn object_at_spanned_reparses_identically() {
        let data = simple_doc("spanned");
        let doc = Document::load(data).unwrap();
        for (num, entry) in doc.xref().iter() {
            let XrefEntry::InFile { offset, gen } = entry else {
                continue;
            };
            let (r, object, span) = doc.object_at_spanned(offset as usize).unwrap();
            assert_eq!(r.num, num);
            assert_eq!(r.gen, gen);
            assert_eq!(span.start, offset);
            assert!(span.end as usize <= doc.bytes().len());
            // The bytes at the span parse back to the same object.
            let slice = &doc.bytes()[span.start as usize..span.end as usize];
            let (r2, object2) = Parser::new(slice).parse_indirect(&NoResolve).unwrap();
            assert_eq!(r2, r);
            assert_eq!(object2, object);
        }
    }

    #[test]
    fn page_object_ref_points_at_a_page_dict() {
        let doc = Document::load(multi_page_doc(&["one", "two"])).unwrap();
        for index in 0..doc.page_count() {
            let page = doc.page(index).unwrap();
            let r = page.object_ref().expect("builder pages are indirect");
            let resolved = doc.get(r).unwrap();
            assert_eq!(
                resolved
                    .as_dict()
                    .unwrap()
                    .get_name("Type")
                    .map(|n| n.0.as_str()),
                Some("Page")
            );
        }
    }

    // --- Minimal Standard-handler (RC4 V2/R3) fixture builder, duplicating
    // the key-derivation mechanism `crypt::tests` uses under the empty user
    // password (those helpers are private to that module's tests). Needed
    // only to pin the decrypt-identity regression test below: RC4's
    // per-object key depends on the object's num/gen, so it is the cipher
    // that can actually distinguish "decrypt with the parsed header's
    // identity" from "decrypt with the caller's requested identity".

    const RC4_FIXTURE_KEY_LEN: usize = 16; // 128-bit key
    const RC4_FIXTURE_P: i32 = -44;
    const RC4_FIXTURE_ID0: &[u8] = b"0123456789abcdef";
    const RC4_FIXTURE_PAD: [u8; 32] = [
        0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01,
        0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53,
        0x69, 0x7A,
    ];

    #[rustfmt::skip]
    const RC4_FIXTURE_MD5_S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    #[rustfmt::skip]
    const RC4_FIXTURE_MD5_K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];

    fn rc4_fixture_md5(input: &[u8]) -> [u8; 16] {
        let (mut a0, mut b0, mut c0, mut d0) = (
            0x6745_2301u32,
            0xefcd_ab89u32,
            0x98ba_dcfeu32,
            0x1032_5476u32,
        );
        let mut msg = input.to_vec();
        let bitlen = (input.len() as u64).wrapping_mul(8);
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bitlen.to_le_bytes());
        for chunk in msg.chunks_exact(64) {
            let mut m = [0u32; 16];
            for (word, bytes) in m.iter_mut().zip(chunk.chunks_exact(4)) {
                *word = u32::from_le_bytes(bytes.try_into().unwrap());
            }
            let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
            for i in 0..64 {
                let (f, g) = match i {
                    0..=15 => ((b & c) | (!b & d), i),
                    16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                    32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (b | !d), (7 * i) % 16),
                };
                let f = f
                    .wrapping_add(a)
                    .wrapping_add(RC4_FIXTURE_MD5_K[i])
                    .wrapping_add(m[g]);
                a = d;
                d = c;
                c = b;
                b = b.wrapping_add(f.rotate_left(RC4_FIXTURE_MD5_S[i]));
            }
            a0 = a0.wrapping_add(a);
            b0 = b0.wrapping_add(b);
            c0 = c0.wrapping_add(c);
            d0 = d0.wrapping_add(d);
        }
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&a0.to_le_bytes());
        out[4..8].copy_from_slice(&b0.to_le_bytes());
        out[8..12].copy_from_slice(&c0.to_le_bytes());
        out[12..16].copy_from_slice(&d0.to_le_bytes());
        out
    }

    fn rc4_fixture_rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        let mut out = Vec::with_capacity(data.len());
        let (mut i, mut j) = (0u8, 0u8);
        for &byte in data {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            let k = s[s[i as usize].wrapping_add(s[j as usize]) as usize];
            out.push(byte ^ k);
        }
        out
    }

    /// `/O` for empty owner and user passwords (Algorithm 3, R3).
    fn rc4_fixture_owner_entry() -> Vec<u8> {
        let mut d = rc4_fixture_md5(&RC4_FIXTURE_PAD);
        for _ in 0..50 {
            d = rc4_fixture_md5(&d[..RC4_FIXTURE_KEY_LEN]);
        }
        let rc4key = d[..RC4_FIXTURE_KEY_LEN].to_vec();
        let mut o = rc4_fixture_rc4(&rc4key, &RC4_FIXTURE_PAD);
        for i in 1u8..=19 {
            let k: Vec<u8> = rc4key.iter().map(|b| b ^ i).collect();
            o = rc4_fixture_rc4(&k, &o);
        }
        o
    }

    /// File key from `/O` for the empty user password (Algorithm 2, R3).
    fn rc4_fixture_file_key(o: &[u8]) -> Vec<u8> {
        let mut input = Vec::new();
        input.extend_from_slice(&RC4_FIXTURE_PAD);
        input.extend_from_slice(o);
        input.extend_from_slice(&(RC4_FIXTURE_P as u32).to_le_bytes());
        input.extend_from_slice(RC4_FIXTURE_ID0);
        let mut d = rc4_fixture_md5(&input);
        for _ in 0..50 {
            d = rc4_fixture_md5(&d[..RC4_FIXTURE_KEY_LEN]);
        }
        d[..RC4_FIXTURE_KEY_LEN].to_vec()
    }

    /// `/U` for the empty user password (Algorithm 5, R3).
    fn rc4_fixture_user_entry(key: &[u8]) -> Vec<u8> {
        let mut input = Vec::new();
        input.extend_from_slice(&RC4_FIXTURE_PAD);
        input.extend_from_slice(RC4_FIXTURE_ID0);
        let mut x = rc4_fixture_md5(&input).to_vec();
        x = rc4_fixture_rc4(key, &x);
        for i in 1u8..=19 {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            x = rc4_fixture_rc4(&k, &x);
        }
        x.resize(32, 0); // trailing padding is arbitrary
        x
    }

    fn rc4_fixture_obj_key(key: &[u8], num: u32, gen: u16) -> Vec<u8> {
        let mut input = key.to_vec();
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        rc4_fixture_md5(&input)[..(key.len() + 5).min(16)].to_vec()
    }

    fn rc4_fixture_hexstr(b: &[u8]) -> String {
        let mut s = String::from("<");
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s.push('>');
        s
    }

    /// Builds a V2/R3 (128-bit RC4) file, encrypted under the empty
    /// password, with a single indirect object (`3 0 obj`, i.e. gen 0)
    /// holding an encrypted string.
    fn rc4_encrypted_fixture() -> Vec<u8> {
        let o = rc4_fixture_owner_entry();
        let key = rc4_fixture_file_key(&o);
        let u = rc4_fixture_user_entry(&key);
        let msg = rc4_fixture_rc4(&rc4_fixture_obj_key(&key, 3, 0), b"Top secret message");

        let mut b = PdfBuilder::new().version(1, 4);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(3, &format!("<< /Msg {} >>", rc4_fixture_hexstr(&msg)));
        b.object(
            9,
            &format!(
                "<< /Filter /Standard /V 2 /R 3 /Length 128 /P {} /O {} /U {} >>",
                RC4_FIXTURE_P,
                rc4_fixture_hexstr(&o),
                rc4_fixture_hexstr(&u)
            ),
        );
        let trailer = format!(
            "/Encrypt 9 0 R /ID [{}{}]",
            rc4_fixture_hexstr(RC4_FIXTURE_ID0),
            rc4_fixture_hexstr(RC4_FIXTURE_ID0)
        );
        b.trailer_extra(&trailer).build(1)
    }

    #[test]
    fn encrypted_generation_mismatch_still_decrypts() {
        // `object_at_spanned` derives the per-object RC4/AESV2 decrypt key
        // from the PARSED "N G obj" header at the object's file offset
        // (`r.num`, `r.gen` from `parser.parse_indirect`), never from the
        // caller's requested `ObjRef` — mirroring how plain (unencrypted)
        // lookups already tolerate a generation mismatch
        // (`generation_mismatch_is_tolerated`). Request object 3 (really
        // "3 0 obj" in the file) under a deliberately wrong generation: if
        // decryption instead used the requested (wrong) gen to derive the
        // RC4 object key, the result would be garbage, not the plaintext.
        let doc = Document::load(rc4_encrypted_fixture()).expect("empty password opens the file");
        let obj3 = doc.get(ObjRef { num: 3, gen: 7 }).unwrap();
        let msg = obj3
            .as_dict()
            .unwrap()
            .get("Msg")
            .unwrap()
            .as_str_bytes()
            .unwrap();
        assert_eq!(
            msg, b"Top secret message",
            "decrypted using the file's real gen (0), not the mismatched request (7)"
        );
    }
}
