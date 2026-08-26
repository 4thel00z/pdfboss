//! The document model: loading, object resolution with caching, the
//! lazily-flattened page tree with attribute inheritance, and document
//! metadata.

use crate::hash::{FastMap, FastSet};
use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crate::crypt::Decryptor;
use crate::elements::Span;
use crate::error::{Error, Result};
use crate::filters;
use crate::geom::Rect;
use crate::object::{decode_text_string, Dict, ObjRef, Object, Stream};
use crate::objstm;
use crate::parser::{Parser, Resolve};
use crate::source::{block_on, AsyncObjectSource, Immediate};
use crate::xref::{load_xref, Xref, XrefEntry};

/// Page-tree traversal depth cap.
const MAX_TREE_DEPTH: usize = 256;

/// A loaded PDF document.
pub struct Document {
    data: Arc<Vec<u8>>,
    version: (u8, u8),
    xref: Arc<Xref>,
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
    /// every page dictionary. See [`Document::pages`]. Shared so a
    /// [`Document::fork`] does not re-walk the tree.
    pages: OnceCell<Arc<Vec<PageRec>>>,
}

/// A document's immutable core, detached from its single-threaded caches:
/// the file bytes, the merged cross-reference table, the decryption key
/// material, and the flattened page tree. `Send + Sync` (asserted in this
/// module's tests), which the [`Document`] deliberately is not — this is
/// the value that crosses a thread boundary, one fresh document
/// materializing from it on each side. See [`Document::seed`],
/// [`Document::from_seed`] and [`map_pages`].
pub struct DocumentSeed {
    data: Arc<Vec<u8>>,
    version: (u8, u8),
    xref: Arc<Xref>,
    decryptor: Option<Decryptor>,
    pages: Arc<Vec<PageRec>>,
}

impl Clone for DocumentSeed {
    fn clone(&self) -> DocumentSeed {
        DocumentSeed {
            data: Arc::clone(&self.data),
            version: self.version,
            xref: Arc::clone(&self.xref),
            decryptor: self.decryptor.clone(),
            pages: Arc::clone(&self.pages),
        }
    }
}

/// The flattened, inheritance-applied record for one page.
struct PageRec {
    obj_ref: Option<ObjRef>,
    media_box: Rect,
    crop_box: Rect,
    bleed_box: Rect,
    trim_box: Rect,
    art_box: Rect,
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
        Document::load_with_password(data, "")
    }

    /// [`Document::load`] with the password that opens the file, accepted as
    /// either the user or the owner password. The empty string is the
    /// transparent empty-user-password case [`Document::load`] always
    /// handles; a password the file does not accept yields
    /// [`Error::Encrypted`]. Non-ASCII passwords are tried UTF-8 encoded
    /// and, for the legacy RC4/AES-128 revisions, Latin-1 encoded as well,
    /// covering both encodings real files use.
    pub fn load_with_password(data: Vec<u8>, password: &str) -> Result<Document> {
        let version = parse_version(&data);
        let xref = load_xref(&data)?;
        let mut doc = Document {
            data: Arc::new(data),
            version,
            xref: Arc::new(xref),
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
            doc.setup_decryption(password)?;
        }
        Ok(doc)
    }

    /// Reads the file at `path` and loads it via
    /// [`Document::load_with_password`].
    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<Document> {
        Document::load_with_password(std::fs::read(path)?, password)
    }

    /// Configures decryption for an encrypted file. Supports the Standard
    /// security handler with RC4 (`/V` 1–2), AESV2 (`/V` 4) and AESV3 (`/V` 5)
    /// with `password` as the user or owner password (the empty string is
    /// the transparent empty-user-password case); a password the file does
    /// not accept is reported as [`Error::Encrypted`]. Must run before any
    /// content object is fetched, and reads `/Encrypt` and `/ID` while
    /// decryption is still off (those values are stored unencrypted).
    fn setup_decryption(&mut self, password: &str) -> Result<()> {
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
        match Decryptor::from_standard_with_password_str(enc_dict, &id0, password) {
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

    /// Chases reference chains with a depth guard of
    /// [`MAX_RESOLVE_DEPTH`](crate::source::MAX_RESOLVE_DEPTH) (beyond that:
    /// [`Error::CircularReference`], naming the last reference followed); a
    /// reference to a missing or unreadable object resolves to `Null`
    /// (lenient).
    ///
    /// The loop is [`crate::source::resolve_sync_with`], shared with the
    /// provided [`crate::source::ObjectSource::resolve`], so the two cannot
    /// drift apart. It reaches `Document::get` through this type's
    /// `ObjectSource` implementation, which forwards to it unchanged.
    pub fn resolve(&self, o: &Object) -> Result<Object> {
        crate::source::resolve_sync_with(self, o)
    }

    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this document.
    pub fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
        filters::decode_stream(s, self)
    }

    /// The flattened page tree, built once on first access.
    fn pages(&self) -> &[PageRec] {
        self.pages.get_or_init(|| Arc::new(self.flatten_pages()))
    }

    /// The document's shareable core: everything immutable, nothing cached.
    /// Forces the page tree first, so no document built from the seed
    /// re-walks it.
    pub fn seed(&self) -> DocumentSeed {
        self.pages();
        DocumentSeed {
            data: Arc::clone(&self.data),
            version: self.version,
            xref: Arc::clone(&self.xref),
            decryptor: self.decryptor.clone(),
            pages: Arc::clone(self.pages.get().expect("pages() was just forced")),
        }
    }

    /// A handle to the same document for another thread: share a
    /// [`DocumentSeed`] across the thread boundary — the seed is `Send` and
    /// `Sync`; the document, whose caches are single-threaded by design, is
    /// neither — and materialize one of these per thread.
    ///
    /// The immutable core is shared — the file bytes, the merged
    /// cross-reference table, the decryption key material, and the flattened
    /// page tree — while the interior caches start fresh, private to this
    /// document. That is the whole design: per-page work is independent and
    /// lock-free precisely because nothing mutable is shared, so N documents
    /// on N threads contend on nothing.
    ///
    /// [`map_pages`] is the ready-made consumer.
    pub fn from_seed(seed: DocumentSeed) -> Document {
        Document {
            data: seed.data,
            version: seed.version,
            xref: seed.xref,
            cache: RefCell::new(FastMap::default()),
            loading: RefCell::new(FastSet::default()),
            objstms: RefCell::new(FastMap::default()),
            decryptor: seed.decryptor,
            pages: OnceCell::from(seed.pages),
        }
    }

    /// [`Document::seed`] and [`Document::from_seed`] in one step, for a
    /// fork used on the calling thread.
    pub fn fork(&self) -> Document {
        Document::from_seed(self.seed())
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
            bleed_box: rec.bleed_box,
            trim_box: rec.trim_box,
            art_box: rec.art_box,
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

    /// The document's optional-content visibility under its default
    /// configuration (ISO 32000-1 §8.11.4.3), or `None` when the catalog
    /// declares no `/OCProperties` — no optional content, everything
    /// visible. Computed per call from a handful of object reads.
    pub fn oc_state(&self) -> Option<crate::oc::OcState> {
        block_on(crate::oc::OcState::load_with(
            &Immediate(self),
            &self.xref.trailer,
        ))
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
                None => {
                    // BleedBox, TrimBox and ArtBox are not inheritable
                    // (ISO 32000 §7.7.3.3, Table 30): read them from the
                    // leaf dictionary only.
                    let bleed = self.rect_value(dict, "BleedBox");
                    let trim = self.rect_value(dict, "TrimBox");
                    let art = self.rect_value(dict, "ArtBox");
                    pages.push(make_page_rec(
                        node_ref,
                        dict.clone(),
                        &inherited,
                        bleed,
                        trim,
                        art,
                    ));
                }
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

/// Applies `work` to every page of `doc` in parallel and returns the
/// results in page order.
///
/// Per-page work over a `Document` — text extraction, rasterization — is
/// independent and CPU-bound, so this fans it out over
/// `std::thread::available_parallelism()` threads, each holding its own
/// [`Document::fork`]: the immutable core is shared, the caches are private,
/// and the workers contend on nothing.
///
/// Workers pull page indexes from a shared counter rather than a static
/// stride. This is not a style choice: pages vary wildly in cost, and on a
/// machine with heterogeneous cores a static partition ends with the fast
/// cores idle while a slow one finishes its stripe — measured as the
/// difference between 3.9x and 5.9x on twelve cores. A counter keeps every
/// worker busy until the pages run out.
///
/// A single-page document, or a single-core machine, runs inline on the
/// calling thread with no fork and no thread.
pub fn map_pages<T, F>(doc: &Document, work: F) -> Vec<Result<T>>
where
    T: Send + Sync,
    F: Fn(&Document, &Page) -> Result<T> + Send + Sync,
{
    let count = doc.pages().len();
    let workers = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(count);
    if workers <= 1 {
        return (0..count)
            .map(|i| doc.page(i).and_then(|page| work(doc, &page)))
            .collect();
    }

    let seed = doc.seed();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::OnceLock<Result<T>>> =
        (0..count).map(|_| std::sync::OnceLock::new()).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            // The seed crosses the thread boundary; the document — whose
            // caches are single-threaded by design — materializes inside.
            let seed = seed.clone();
            let next = &next;
            let slots = &slots;
            let work = &work;
            scope.spawn(move || {
                let worker = Document::from_seed(seed);
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= count {
                        break;
                    }
                    let outcome = worker.page(i).and_then(|page| work(&worker, &page));
                    // Each index is handed to exactly one worker, so the
                    // slot is always empty; a failed set would mean the
                    // counter duplicated an index, which is worth crashing
                    // over.
                    if slots[i].set(outcome).is_err() {
                        unreachable!("page index {i} was dispatched twice");
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("every page index below the count was dispatched")
        })
        .collect()
}

/// Builds the final page record from a leaf dictionary and its inherited
/// attributes. The defaults live in [`Page::from_tree_attrs`] — the one
/// implementation of page defaulting, shared with the asynchronous API —
/// and this only reshapes its output into the index-less cache record.
fn make_page_rec(
    obj_ref: Option<ObjRef>,
    dict: Dict,
    inherited: &Inherited,
    bleed_box: Option<Rect>,
    trim_box: Option<Rect>,
    art_box: Option<Rect>,
) -> PageRec {
    let page = Page::from_tree_attrs(
        0,
        inherited.resources.clone(),
        inherited.media_box,
        inherited.crop_box,
        bleed_box,
        trim_box,
        art_box,
        inherited.rotate,
        dict,
        obj_ref,
    );
    PageRec {
        obj_ref: page.obj_ref,
        media_box: page.media_box,
        crop_box: page.crop_box,
        bleed_box: page.bleed_box,
        trim_box: page.trim_box,
        art_box: page.art_box,
        rotate: page.rotate,
        resources: page.resources,
        dict: page.dict,
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

/// Forwards to the inherent methods, so reading a document through the trait
/// is bit-identical to reading it directly. `resolve` is forwarded too, even
/// though the provided implementation is now the same shared chase
/// ([`crate::source::resolve_sync_with`]) that the inherent method delegates
/// to: routing it through the inherent method keeps the two entry points a
/// single call, so they cannot drift should `Document::resolve` ever grow
/// document-specific behaviour.
impl crate::source::ObjectSource for Document {
    fn get(&self, r: ObjRef) -> Result<Object> {
        Document::get(self, r)
    }

    fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
        Document::stream_data(self, s)
    }

    fn resolve(&self, o: &Object) -> Result<Object> {
        Document::resolve(self, o)
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
/// `bleed_box`, `trim_box` and `art_box` fall back to `crop_box` (and are
/// clipped to `media_box`) per ISO 32000 §14.11.2, and `rotate` is
/// normalized to one of {0, 90, 180, 270}. Every construction path
/// normalizes `rotate` — [`Document::page`] and [`Page::from_parts`] alike —
/// so the invariant holds however a `Page` was built. `rotate` is a public
/// field, so a caller may still overwrite it afterwards; [`Page::size`]
/// reads it modulo a full turn and so stays correct if they do.
///
/// All five boxes are in unrotated PDF user space: [`Page::size`] swaps
/// width and height under a quarter-turn `/Rotate`, the boxes never do.
#[derive(Clone)]
pub struct Page {
    /// 0-based page index.
    pub index: usize,
    pub media_box: Rect,
    pub crop_box: Rect,
    pub bleed_box: Rect,
    pub trim_box: Rect,
    pub art_box: Rect,
    pub rotate: i32,
    /// The page's (inherited) `/Resources` dictionary.
    pub resources: Dict,
    dict: Dict,
    obj_ref: Option<ObjRef>,
}

impl Page {
    /// Builds a page from already-resolved attributes.
    ///
    /// [`Document::page`] resolves page-tree inheritance itself; this is for
    /// a caller that has done that resolution some other way — notably the
    /// asynchronous API, which flattens the page tree while reading it — and
    /// needs the same [`Page`] type back.
    ///
    /// `rotate` is normalized here to one of `{0, 90, 180, 270}` exactly as
    /// [`Document::page`] normalizes it, so a caller may pass a raw `/Rotate`
    /// straight through: a negative or over-a-full-turn multiple of 90 is
    /// reduced, and a value that is not a multiple of 90 falls back to 0
    /// (lenient). The caller does not have to pre-normalize, and the [`Page`]
    /// invariant holds whichever constructor built it.
    ///
    /// `bleed_box`, `trim_box` and `art_box` are set to `crop_box` — their
    /// ISO 32000 §14.11.2 default. A caller that resolved real values
    /// overwrites the public fields afterwards.
    pub fn from_parts(
        index: usize,
        media_box: Rect,
        crop_box: Rect,
        rotate: i32,
        resources: Dict,
        dict: Dict,
        obj_ref: Option<ObjRef>,
    ) -> Page {
        Page {
            index,
            media_box,
            crop_box,
            bleed_box: crop_box,
            trim_box: crop_box,
            art_box: crop_box,
            rotate: normalize_rotation(rotate),
            resources,
            dict,
            obj_ref,
        }
    }

    /// The default media box for a page that declares none: US Letter
    /// (612 by 792 points). Public because the default is part of the
    /// observable contract — a caller comparing two APIs' pages needs to
    /// know which rectangle "the file said nothing" maps to.
    pub const US_LETTER: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);

    /// Builds a page from raw, possibly missing page-tree attributes,
    /// applying the same defaults [`Document::page`] applies (ISO 32000
    /// §7.7.3.3, §14.11.2): a missing or degenerate `/MediaBox` reads as
    /// [`Page::US_LETTER`], `/CropBox` clips to the media box and falls back
    /// to it, `/BleedBox`, `/TrimBox` and `/ArtBox` clip to the media box
    /// and fall back to the crop box, a missing `/Rotate` reads as 0 and is
    /// normalized, and missing `/Resources` read as empty.
    ///
    /// This is the one implementation of page defaulting. The synchronous
    /// tree walk routes through it, and a caller that resolved inheritance
    /// some other way — notably the asynchronous API, which flattens the
    /// page tree while reading it — gets the identical `Page` back, so the
    /// two APIs cannot disagree about what an attribute-less page looks
    /// like.
    #[allow(clippy::too_many_arguments)]
    pub fn from_tree_attrs(
        index: usize,
        resources: Option<Dict>,
        media_box: Option<Rect>,
        crop_box: Option<Rect>,
        bleed_box: Option<Rect>,
        trim_box: Option<Rect>,
        art_box: Option<Rect>,
        rotate: Option<i32>,
        dict: Dict,
        obj_ref: Option<ObjRef>,
    ) -> Page {
        let media_box = media_box
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .unwrap_or(Page::US_LETTER);
        let crop_box = crop_box
            .and_then(|c| c.intersect(media_box))
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .unwrap_or(media_box);
        let clip = |declared: Option<Rect>| {
            declared
                .and_then(|b| b.intersect(media_box))
                .filter(|r| r.width() > 0.0 && r.height() > 0.0)
                .unwrap_or(crop_box)
        };
        Page {
            index,
            media_box,
            crop_box,
            bleed_box: clip(bleed_box),
            trim_box: clip(trim_box),
            art_box: clip(art_box),
            rotate: normalize_rotation(rotate.unwrap_or(0)),
            resources: resources.unwrap_or_default(),
            dict,
            obj_ref,
        }
    }

    /// The page's indirect object reference, when the page came from an
    /// indirect kid in the page tree (pages inlined directly into a `/Kids`
    /// array have none).
    pub fn object_ref(&self) -> Option<ObjRef> {
        self.obj_ref
    }

    /// The page's decoded content: the `/Contents` stream, or all streams
    /// of a `/Contents` array decoded and joined with `b"\n"`. A missing
    /// `/Contents` yields empty content (lenient).
    ///
    /// This drives [`page_content_with`] to completion on the calling thread.
    /// There is one implementation of the algorithm, so this and the
    /// asynchronous API cannot drift apart in what they consider a page's
    /// content to be.
    pub fn content(&self, doc: &Document) -> Result<Vec<u8>> {
        block_on(page_content_with(Immediate(doc), self))
    }

    /// Crop-box width and height, swapped when `/Rotate` is a quarter turn.
    ///
    /// Both constructors normalize `rotate`, but the field is public and so
    /// can be overwritten with a raw `/Rotate`; the rotation is therefore read
    /// modulo a full turn rather than compared against 90 and 270 exactly, so
    /// that -90 and 450 swap the dimensions just as 270 and 90 do. For an
    /// already-normalized value this is the same test as before: 0, 90, 180
    /// and 270 are unchanged by `rem_euclid(360)`.
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        let turn = self.rotate.rem_euclid(360);
        if turn == 90 || turn == 270 {
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

/// A page's decoded content, awaiting each fetch: the `/Contents` stream, or
/// all streams of a `/Contents` array decoded and joined with `b"\n"`. A
/// missing `/Contents` yields empty content (lenient).
///
/// This is the only implementation of that algorithm. [`Page::content`] is a
/// [`block_on`] wrapper over it, which is what stops the synchronous and
/// asynchronous APIs from disagreeing about what a page's content is.
///
/// # Choosing this signature
///
/// `src` is taken **by value** rather than by reference. A future holding
/// `&'a S` is `Send` but never `'static`, and the asynchronous consumers —
/// spawning onto a runtime, or crossing into the Python bindings — need both.
/// By value costs nothing in practice: an asynchronous document is an `Arc`
/// handle, and [`Immediate`] over a borrowed document is `Copy`.
///
/// There is deliberately **no `Send` or `Sync` bound** on `S`. Auto traits are
/// inferred per instantiation, so this one function yields a `Send` future over
/// a genuinely asynchronous source and a non-`Send` future over
/// `Immediate<&Document>` — which is what is wanted, because [`block_on`]
/// drives the latter on the calling thread and never sends it anywhere. A
/// `S: Sync` bound would exclude `Immediate<&Document>` outright; that is also
/// why this calls `src.resolve` directly rather than
/// [`crate::source::resolve_with`], which does require `Sync`.
///
/// The returned future is `'static` only when the caller owns the [`Page`]
/// inside its own `async` block — the `&Page` borrow is what otherwise
/// prevents it.
///
/// # Errors
///
/// Propagates whatever `src` reports for a fetch or a stream decode.
pub async fn page_content_with<S: AsyncObjectSource>(src: S, page: &Page) -> Result<Vec<u8>> {
    let Some(contents) = page.dict.get("Contents") else {
        return Ok(Vec::new());
    };
    match src.resolve(contents).await? {
        Object::Stream(ref s) => content_stream_data_with(&src, s).await,
        Object::Array(items) => {
            let mut out = Vec::new();
            let mut first = true;
            for item in &items {
                let part = src.resolve(item).await?;
                let Some(stream) = part.as_stream() else {
                    continue; // non-stream entries are skipped (lenient)
                };
                if !first {
                    out.push(b'\n');
                }
                out.extend_from_slice(&content_stream_data_with(&src, stream).await?);
                first = false;
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// Fetches one stream's bytes GUARANTEED DECODED — refusing the streams
/// whose bytes `decode_stream` leaves encoded for the image layer. This is
/// the fetch for **every consumer that is not an image decoder**: content
/// streams, font programs, `/ToUnicode` CMaps, `/CIDToGIDMap` tables —
/// anything that parses what it fetches.
///
/// This is [`AsyncObjectSource::stream_data`] with two refusals in front.
/// A stream whose trailing `/Filter` entry is an image codec (`DCTDecode`,
/// `JPXDecode`) fails with [`Error::UnsupportedFilter`] instead of handing
/// back the passthrough (ISO 32000-1 7.4.9): a raw JPEG or JPEG 2000
/// codestream is indistinguishable from decoded data to anything that is
/// not an image decoder, and a parser fed one chews binary garbage into
/// operators, tables, or mappings with a clean result. And a `/Filter`
/// that cannot be READ at all — a reference cycle, or a chain deeper than
/// [`crate::source::MAX_RESOLVE_DEPTH`] — is refused with the resolve
/// error, because a value that cannot be read might name an image codec,
/// and `decode_stream` would leniently return the bytes as stored. Either
/// refusal is the same reportable error as any other filter this library
/// cannot run.
///
/// One leniency is kept, deliberately: a `/Filter` that READS as `null`
/// (a dangling reference included — ISO 32000-1 7.3.10 makes one
/// equivalent to `null`) or as an unusable value says "no filter", which
/// is exactly what `decode_stream` does with it; the stored bytes are the
/// decoded bytes by that reading, and no codec name can hide in a value
/// that is fully visible.
///
/// Raw [`AsyncObjectSource::stream_data`] remains correct in exactly one
/// place: the image layer, which reads the trailing filter itself and
/// decodes the passthrough. A new call site that is not decoding images
/// belongs here instead. Same calling convention as [`page_content_with`]
/// (`src` by value; see that function's docs).
pub async fn decoded_stream_data_with<S: AsyncObjectSource>(src: S, s: &Stream) -> Result<Vec<u8>> {
    if let Some(name) = filters::trailing_filter_checked_with(&src, &s.dict).await? {
        if filters::is_image_codec(&name.0) {
            return Err(Error::UnsupportedFilter(name.0));
        }
    }
    src.stream_data(s).await
}

/// Fetches and decodes one CONTENT stream — page `/Contents`, a form
/// XObject, a Type3 CharProc, a pattern cell: anything whose bytes feed a
/// content parser rather than an image decoder.
///
/// The content-flavored name for [`decoded_stream_data_with`], which is
/// the same refusal for every non-image consumer; see it for why a
/// passthrough image codec must never reach a parser. For a content
/// stream the refusal is what turns the mislabelled stream into the same
/// reported skip as any other filter this library cannot run.
pub async fn content_stream_data_with<S: AsyncObjectSource>(src: S, s: &Stream) -> Result<Vec<u8>> {
    decoded_stream_data_with(src, s).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;
    use crate::parser::{NoResolve, Parser};
    use crate::xref::XrefEntry;
    use pdfboss_testkit::{multi_page_doc, objstm_doc, objstm_payload, simple_doc, PdfBuilder};

    /// The synchronous accessor delegates to the asynchronous one, so the two
    /// cannot report different content for the same page. This asserts the
    /// equality directly rather than trusting the delegation to stay in place.
    #[test]
    fn async_page_content_matches_the_sync_accessor() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).expect("load");
        let mut saw_content = false;
        for index in 0..3 {
            let page = doc.page(index).expect("page");
            let direct = page.content(&doc).expect("sync content");
            let awaited =
                block_on(page_content_with(Immediate(&doc), &page)).expect("async content");
            assert_eq!(direct, awaited, "page {index}");
            saw_content |= !direct.is_empty();
        }
        // Without this the loop above would pass on three empty vectors.
        assert!(
            saw_content,
            "fixture must give at least one page real content"
        );
    }

    /// A page `/Contents` stream whose trailing filter is an image codec is
    /// refused, never parsed: `decode_stream` would pass its bytes through
    /// STILL ENCODED (ISO 32000-1 7.4.9 reserves that for the image layer),
    /// and the stream below is deliberately valid operator syntax to prove
    /// the refusal happens on the label, not on the bytes. The error is the
    /// same `UnsupportedFilter` any genuinely undecodable filter raises, so
    /// every caller reports it identically.
    #[test]
    fn image_codec_page_contents_are_refused_not_returned() {
        for codec in ["JPXDecode", "DCTDecode"] {
            let mut b = PdfBuilder::new();
            b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
            b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
            b.object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
            );
            b.stream(
                4,
                &format!("/Filter /{codec}"),
                b"1 0 0 rg 0 0 100 100 re f",
            );
            let doc = Document::load(b.build(1)).expect("load");
            let page = doc.page(0).expect("page");
            match page.content(&doc) {
                Err(Error::UnsupportedFilter(n)) => assert_eq!(n, codec),
                other => panic!("{codec} content must be refused, got {other:?}"),
            }
        }
    }

    /// A `/Contents` whose `/Filter` is a reference CYCLE is refused as
    /// unreadable, never fetched: `decode_stream` cannot resolve the value
    /// either and would leniently return the bytes AS STORED — and a value
    /// nobody can read might name an image codec, so the stored bytes may
    /// be a passthrough codestream. The bytes below are deliberately valid
    /// operator syntax to prove the refusal is about the unreadable label.
    #[test]
    fn an_unreadable_filter_refuses_the_content_not_returns_it_raw() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        );
        b.stream(4, "/Filter 6 0 R", b"1 0 0 rg 0 0 100 100 re f");
        b.object(6, "7 0 R");
        b.object(7, "6 0 R");
        let doc = Document::load(b.build(1)).expect("load");
        let page = doc.page(0).expect("page");
        assert!(
            page.content(&doc).is_err(),
            "an unreadable /Filter must refuse, not pass stored bytes"
        );
    }

    /// The composition every page-reading algorithm actually uses: the caller
    /// owns its source so that its own future can be `'static`, and reaches this
    /// helper — which owns its source for the same reason — by handing out a
    /// reference. That works because `&S` is itself an `AsyncObjectSource`.
    #[test]
    fn a_shared_helper_is_reachable_from_a_caller_that_owns_its_source() {
        let doc = Document::load(simple_doc("Hello")).expect("load");
        let page = doc.page(0).expect("page");

        let owner = Immediate(&doc);
        let through_reference = block_on(page_content_with(&owner, &page)).expect("async content");

        assert_eq!(through_reference, page.content(&doc).expect("sync content"));
        assert!(!through_reference.is_empty());
    }

    /// The seed is what crosses a thread boundary, so this is the compile
    /// gate for the whole fan-out design. The `Document` itself must stay
    /// out of this list: its caches are single-threaded by design.
    #[test]
    fn the_seed_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DocumentSeed>();
    }

    /// A fork shares the file and the page tree but none of the caches, and
    /// reads identically to its parent — including through decryption, whose
    /// key material is part of the shared immutable core.
    #[test]
    fn a_fork_reads_exactly_what_its_parent_reads() {
        let doc = Document::load(pdfboss_testkit::encrypted_rc4_doc("forked secret")).unwrap();
        let fork = doc.fork();
        assert_eq!(fork.page_count(), doc.page_count());
        let (a, b) = (doc.page(0).unwrap(), fork.page(0).unwrap());
        assert_eq!(a.media_box, b.media_box);
        assert_eq!(
            a.content(&doc).expect("parent content"),
            b.content(&fork).expect("fork content"),
            "decrypted content agrees"
        );
    }

    /// The results come back in page order whatever order the workers finish
    /// in, and a per-page error occupies its own slot without disturbing the
    /// others.
    #[test]
    fn map_pages_preserves_page_order() {
        let doc = Document::load(multi_page_doc(&["one", "two", "three"])).unwrap();
        let contents = map_pages(&doc, |doc, page| page.content(doc));
        assert_eq!(contents.len(), 3);
        let texts: Vec<String> = contents
            .into_iter()
            .map(|c| String::from_utf8_lossy(&c.unwrap()).into_owned())
            .collect();
        assert!(texts[0].contains("(one)"), "{}", texts[0]);
        assert!(texts[1].contains("(two)"), "{}", texts[1]);
        assert!(texts[2].contains("(three)"), "{}", texts[2]);
    }

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
        assert_eq!(
            odd.media_box,
            Page::US_LETTER,
            "degenerate media box defaults"
        );
    }

    #[test]
    fn page_boxes_declared_and_defaulted() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] \
             /CropBox [50 50 550 750] /BleedBox [40 40 560 760] \
             /TrimBox [5 0 R 6 0 R 7 0 R 8 0 R] >>",
        );
        b.object(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] >>");
        b.object(5, "60");
        b.object(6, "70");
        b.object(7, "540");
        b.object(8, "730");
        let doc = Document::load(b.build(1)).unwrap();

        let page = doc.page(0).unwrap();
        assert_eq!(page.bleed_box, Rect::new(40.0, 40.0, 560.0, 760.0));
        assert_eq!(
            page.trim_box,
            Rect::new(60.0, 70.0, 540.0, 730.0),
            "indirect array elements resolve"
        );
        assert_eq!(
            page.art_box, page.crop_box,
            "undeclared box is the crop box"
        );

        let bare = doc.page(1).unwrap();
        assert_eq!(bare.bleed_box, bare.crop_box);
        assert_eq!(bare.trim_box, bare.crop_box);
        assert_eq!(bare.art_box, bare.crop_box);
    }

    #[test]
    fn page_boxes_clip_to_media_and_fall_back() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /CropBox [10 10 190 190] /TrimBox [100 100 400 400] \
             /BleedBox [0 0 0 0] /ArtBox [300 300 400 400] >>",
        );
        let doc = Document::load(b.build(1)).unwrap();

        let page = doc.page(0).unwrap();
        assert_eq!(
            page.trim_box,
            Rect::new(100.0, 100.0, 200.0, 200.0),
            "a trim box past the media box clips to it"
        );
        assert_eq!(
            page.bleed_box, page.crop_box,
            "a degenerate bleed box falls back to the crop box"
        );
        assert_eq!(
            page.art_box, page.crop_box,
            "an art box disjoint from the media box falls back to the crop box"
        );
    }

    /// `/BleedBox`, `/TrimBox` and `/ArtBox` are not in ISO 32000 Table 30's
    /// inheritable set: a value on a `/Pages` node must not leak into its
    /// leaves, which read their spec default (the crop box) instead.
    #[test]
    fn page_boxes_are_not_inherited() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 \
             /TrimBox [10 10 90 90] /BleedBox [5 5 95 95] /ArtBox [20 20 80 80] >>",
        );
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();

        let page = doc.page(0).unwrap();
        assert_eq!(page.trim_box, page.crop_box);
        assert_eq!(page.bleed_box, page.crop_box);
        assert_eq!(page.art_box, page.crop_box);
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
        for chunk in msg.as_chunks::<64>().0 {
            let mut m = [0u32; 16];
            for (word, bytes) in m.iter_mut().zip(chunk.as_chunks::<4>().0) {
                *word = u32::from_le_bytes(*bytes);
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

    #[test]
    fn objstm_doc_fixture_loads_and_resolves_members() {
        let data = objstm_doc(&[(7, "<< /Marker (inside) >>")]);
        let doc = Document::load(data).unwrap();
        assert_eq!(doc.page_count(), 1);
        let member = doc.get(ObjRef { num: 7, gen: 0 }).unwrap();
        let text = member.as_dict().unwrap().get("Marker").unwrap();
        assert_eq!(text.as_str_bytes(), Some(&b"inside"[..]));
        // The member really is xref'd into the object stream.
        assert!(matches!(
            doc.xref().get(7),
            Some(XrefEntry::InStream { stream_num: 4, .. })
        ));
    }

    /// A page built from parts must expose the same accessors as one that
    /// came out of the page tree — this is the constructor aio uses.
    #[test]
    fn page_from_parts_exposes_its_accessors() {
        let mut dict = Dict::default();
        dict.insert(Name("Type".into()), Object::Name(Name("Page".into())));
        let media = Rect::new(0.0, 0.0, 200.0, 400.0);
        let obj_ref = ObjRef { num: 7, gen: 0 };

        let page = Page::from_parts(
            3,
            media,
            media,
            90,
            Dict::default(),
            dict.clone(),
            Some(obj_ref),
        );

        assert_eq!(page.index, 3);
        assert_eq!(page.object_ref(), Some(obj_ref));
        assert_eq!(page.dict(), &dict);
        // /Rotate 90 swaps the reported page size.
        assert_eq!(page.size(), (400.0, 200.0));
        // The boxes from_parts does not take read as their spec default.
        assert_eq!(page.bleed_box, page.crop_box);
        assert_eq!(page.trim_box, page.crop_box);
        assert_eq!(page.art_box, page.crop_box);
    }

    /// `from_parts` normalizes `/Rotate` just as the page tree does, so an
    /// unnormalized quarter-turn stores — and reports the size of — its
    /// canonical equivalent.
    #[test]
    fn page_from_parts_normalizes_rotation() {
        let media = Rect::new(0.0, 0.0, 200.0, 400.0);
        let build = |rotate: i32| {
            Page::from_parts(
                0,
                media,
                media,
                rotate,
                Dict::default(),
                Dict::default(),
                None,
            )
        };

        for (given, canonical) in [(-90, 270), (450, 90), (540, 180), (720, 0), (-360, 0)] {
            let page = build(given);
            assert_eq!(
                page.rotate, canonical,
                "from_parts must normalize /Rotate {given} to {canonical}"
            );
            assert_eq!(
                page.size(),
                build(canonical).size(),
                "/Rotate {given} must report the same size as {canonical}"
            );
        }
    }

    /// `rotate` is a public field, so `size()` cannot rely on the constructor
    /// having normalized it: any multiple of 90 must be read modulo a full
    /// turn. The already-canonical values are listed too, pinning that this
    /// is unchanged for them.
    #[test]
    fn page_size_handles_unnormalized_rotation() {
        let media = Rect::new(0.0, 0.0, 200.0, 400.0);
        let mut page = Page::from_parts(0, media, media, 0, Dict::default(), Dict::default(), None);

        for rotate in [90, 270, -90, -270, 450, 630] {
            page.rotate = rotate;
            assert_eq!(
                page.size(),
                (400.0, 200.0),
                "/Rotate {rotate} is a quarter turn and must swap the size"
            );
        }
        for rotate in [0, 180, -180, 360, 540, 720] {
            page.rotate = rotate;
            assert_eq!(
                page.size(),
                (200.0, 400.0),
                "/Rotate {rotate} is a half turn and must leave the size alone"
            );
        }
    }
}
