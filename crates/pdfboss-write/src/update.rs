//! Incremental updates to an existing file (ISO 32000-1 §7.5.6): the base
//! bytes stay in place and an update section appends new and replaced
//! objects plus a cross-reference section chained to the base's by `/Prev`,
//! in the base's own cross-reference style.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdfboss_core::crypt::Sha256;
use pdfboss_core::xref::{parse_section_at, startxref};
use pdfboss_core::{Dict, Document, FastMap, Name, ObjRef, Object, Stream, XrefKind};

use crate::error::{Error, Result};
use crate::ser::{serialize_dict, serialize_object};
use crate::writer::{WriteOptions, Writer, XrefStyle};

/// The resource name every page draws the overlay form under.
const FORM_NAME: &str = "PdfbossWatermark";

/// Like [`watermark`], but writes a fresh file through the [`Writer`] under
/// `options` instead of appending an update: every object the base's
/// catalog reaches is copied over, uncompressed streams are compressed when
/// `options.compress` is set, and unreachable objects and earlier sections
/// are left behind, so the result is usually smaller than the base.
pub fn watermark_with(
    base: &Document,
    overlay: &Document,
    options: WriteOptions,
) -> Result<Vec<u8>> {
    let trailer = &base.xref().trailer;
    if trailer.get("Encrypt").is_some() {
        return Err(Error::EncryptedBase);
    }
    let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
    let mut writer = Writer::new(options);
    let prefix = writer.put_stream_raw(Dict::new(), b"q\n".to_vec());
    let suffix = writer.put_stream_raw(
        Dict::new(),
        format!("Q\nq /{FORM_NAME} Do Q\n").into_bytes(),
    );
    let mut overlay_copy = Rewrite::new(overlay, options.compress);
    let form = overlay_copy.form(&mut writer)?;
    overlay_copy.drain(&mut writer, None)?;

    let pages: FastMap<ObjRef, usize> = (0..base.page_count())
        .filter_map(|index| {
            let page = base.page(index).ok()?;
            page.object_ref().map(|r| (r, index))
        })
        .collect();
    let stamp = Stamp {
        form,
        prefix,
        suffix,
        pages,
    };
    let mut base_copy = Rewrite::new(base, options.compress);
    let new_root = base_copy.reference(&mut writer, root);
    if let Some(info) = trailer.get_ref("Info") {
        let new_info = base_copy.reference(&mut writer, info);
        writer.set_info(new_info);
    }
    base_copy.drain(&mut writer, Some(&stamp))?;
    writer.finish(new_root)
}

/// What every stamped page draws: the overlay form and the two content
/// streams wrapped around the page's own, plus which objects are pages.
struct Stamp {
    form: ObjRef,
    prefix: ObjRef,
    suffix: ObjRef,
    pages: FastMap<ObjRef, usize>,
}

/// Copies one document's object graph into a [`Writer`]: every reference
/// met is reserved a number once and queued, and the queue is drained
/// iteratively, so a long chain of references costs no stack.
struct Rewrite<'a> {
    source: &'a Document,
    map: FastMap<ObjRef, ObjRef>,
    pending: Vec<ObjRef>,
    compress: bool,
}

impl<'a> Rewrite<'a> {
    fn new(source: &'a Document, compress: bool) -> Rewrite<'a> {
        Rewrite {
            source,
            map: FastMap::default(),
            pending: Vec::new(),
            compress,
        }
    }

    /// The target number for source reference `r`, reserved and queued on
    /// first sight.
    fn reference(&mut self, writer: &mut Writer, r: ObjRef) -> ObjRef {
        if let Some(copied) = self.map.get(&r) {
            return *copied;
        }
        let copied = writer.reserve();
        self.map.insert(r, copied);
        self.pending.push(r);
        copied
    }

    /// A copy of `obj`'s direct structure with every reference mapped.
    fn copy(&mut self, writer: &mut Writer, obj: &Object) -> Result<Object> {
        Ok(match obj {
            Object::Ref(r) => Object::Ref(self.reference(writer, *r)),
            Object::Dict(d) => Object::Dict(self.copy_dict(writer, d)?),
            Object::Array(items) => Object::Array(
                items
                    .iter()
                    .map(|item| self.copy(writer, item))
                    .collect::<Result<Vec<Object>>>()?,
            ),
            Object::Stream(_) => return Err(Error::NestedStream),
            other => other.clone(),
        })
    }

    fn copy_dict(&mut self, writer: &mut Writer, dict: &Dict) -> Result<Dict> {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            out.insert(key.clone(), self.copy(writer, value)?);
        }
        Ok(out)
    }

    /// A stream body: its dictionary copied without `/Length` (the writer
    /// sets it), its data compressed when asked and not already filtered.
    fn copy_stream(&mut self, writer: &mut Writer, stream: &Stream) -> Result<Object> {
        let mut dict = stream.dict.clone();
        dict.remove("Length");
        let mut dict = self.copy_dict(writer, &dict)?;
        let data = if self.compress && dict.get("Filter").is_none() {
            dict.insert(name("Filter"), Object::Name(name("FlateDecode")));
            deflate(&stream.data)
        } else {
            stream.data.clone()
        };
        Ok(Object::Stream(Stream { dict, data }))
    }

    /// Fills every queued object, stamping the pages `stamp` names.
    fn drain(&mut self, writer: &mut Writer, stamp: Option<&Stamp>) -> Result<()> {
        while let Some(r) = self.pending.pop() {
            let target = self.map[&r];
            let stamped = stamp.and_then(|s| s.pages.get(&r).map(|index| (s, *index)));
            let body = match stamped {
                Some((s, index)) => self.stamped_page(writer, index, s)?,
                None => match self.source.get(r).map_err(core_error)? {
                    Object::Stream(s) => self.copy_stream(writer, &s)?,
                    other => self.copy(writer, &other)?,
                },
            };
            writer.fill(target, body)?;
        }
        Ok(())
    }

    /// Page `index`'s dictionary copied with the stamp applied: its
    /// effective resources gain the form, its content is wrapped in the
    /// prefix and suffix streams.
    fn stamped_page(&mut self, writer: &mut Writer, index: usize, stamp: &Stamp) -> Result<Object> {
        let page = self.source.page(index).map_err(core_error)?;
        let mut dict = self.copy_dict(writer, page.dict())?;
        let mut resources = self.copy_dict(writer, &page.resources)?;
        let mut xobjects = match page.resources.get("XObject") {
            Some(existing) => {
                let existing = self.source.resolve(existing).map_err(core_error)?;
                match existing.as_dict() {
                    Some(d) => self.copy_dict(writer, d)?,
                    None => Dict::new(),
                }
            }
            None => Dict::new(),
        };
        xobjects.insert(name(FORM_NAME), Object::Ref(stamp.form));
        resources.insert(name("XObject"), Object::Dict(xobjects));
        dict.insert(name("Resources"), Object::Dict(resources));
        let mut contents = vec![Object::Ref(stamp.prefix)];
        match page.dict().get("Contents") {
            Some(Object::Array(items)) => {
                for item in items {
                    contents.push(self.copy(writer, item)?);
                }
            }
            Some(Object::Ref(r)) => match self.source.get(*r).map_err(core_error)? {
                Object::Array(items) => {
                    for item in &items {
                        contents.push(self.copy(writer, item)?);
                    }
                }
                _ => contents.push(Object::Ref(self.reference(writer, *r))),
            },
            _ => {}
        }
        contents.push(Object::Ref(stamp.suffix));
        dict.insert(name("Contents"), Object::Array(contents));
        Ok(Object::Dict(dict))
    }

    /// The source's first page as a form XObject, filled into the writer:
    /// its media box as the bounding box, its decoded content deflated, its
    /// resources copied.
    fn form(&mut self, writer: &mut Writer) -> Result<ObjRef> {
        let page = self.source.page(0).map_err(core_error)?;
        let content = page.content(self.source).map_err(core_error)?;
        let resources = self.copy_dict(writer, &page.resources)?;
        let bbox = page.media_box;
        let mut dict = Dict::new();
        dict.insert(name("Type"), Object::Name(name("XObject")));
        dict.insert(name("Subtype"), Object::Name(name("Form")));
        dict.insert(name("FormType"), Object::Int(1));
        dict.insert(
            name("BBox"),
            Object::Array(
                [bbox.x0, bbox.y0, bbox.x1, bbox.y1]
                    .iter()
                    .map(|v| Object::Real(f64::from(*v)))
                    .collect(),
            ),
        );
        dict.insert(name("Resources"), Object::Dict(resources));
        dict.insert(name("Filter"), Object::Name(name("FlateDecode")));
        let form = writer.reserve();
        writer.fill(
            form,
            Object::Stream(Stream {
                dict,
                data: deflate(&content),
            }),
        )?;
        Ok(form)
    }
}

/// Draws the first page of `overlay` over every page of `base`, returning
/// `base`'s bytes followed by an incremental update: the overlay page as
/// one form XObject (its resources copied into the base's object space),
/// and each page's dictionary rewritten with that form in its resources
/// and its content wrapped in `q … Q` before the form is drawn. Pages
/// inlined directly into `/Kids`, having no object of their own, are left
/// as they are. An encrypted base is refused: its new strings and streams
/// would need encrypting too.
pub fn watermark(base: &Document, overlay: &Document) -> Result<Vec<u8>> {
    let mut update = Update::new(base)?;
    let form = update.overlay.import_form(overlay)?;
    let prefix = update
        .overlay
        .put(Object::Stream(plain_stream(b"q\n".to_vec())));
    let suffix = update.overlay.put(Object::Stream(plain_stream(
        format!("Q\nq /{FORM_NAME} Do Q\n").into_bytes(),
    )));
    for index in 0..base.page_count() {
        let page = base.page(index).map_err(core_error)?;
        let Some(page_ref) = page.object_ref() else {
            continue;
        };
        let mut dict = page.dict().clone();
        let mut resources = page.resources.clone();
        let mut xobjects = match resources.get("XObject") {
            Some(existing) => base
                .resolve(existing)
                .map_err(core_error)?
                .as_dict()
                .cloned()
                .unwrap_or_default(),
            None => Dict::new(),
        };
        xobjects.insert(name(FORM_NAME), Object::Ref(form));
        resources.insert(name("XObject"), Object::Dict(xobjects));
        dict.insert(name("Resources"), Object::Dict(resources));
        let mut contents = vec![Object::Ref(prefix)];
        match dict.get("Contents").cloned() {
            Some(Object::Array(items)) => contents.extend(items),
            Some(Object::Ref(r)) => match base.get(r).map_err(core_error)? {
                Object::Array(items) => contents.extend(items),
                _ => contents.push(Object::Ref(r)),
            },
            _ => {}
        }
        contents.push(Object::Ref(suffix));
        dict.insert(name("Contents"), Object::Array(contents));
        update.set(page_ref, Object::Dict(dict));
    }
    update.appended()
}

/// The facts about a base document an update needs, read once from its
/// trailer and its own newest cross-reference section: refuses an
/// encrypted base or one missing `/Root` or a `startxref` to chain from.
#[derive(Debug, Clone)]
pub struct OverlayBase {
    /// Byte offset of the base's own newest cross-reference section, named
    /// as the appended section's `/Prev`.
    pub prev: u64,
    /// Style of that newest section, read from the section itself rather
    /// than the merged trailer (a hybrid base's merged trailer carries
    /// `/Type /XRef` inherited from its `/XRefStm`, even though its newest
    /// section, per `startxref`, is the classic table).
    pub kind: XrefStyle,
    /// The next free object number: the base's declared `/Size`, raised to
    /// one past its highest addressed object number.
    pub size: u32,
    /// The base's catalog.
    pub root: ObjRef,
    /// The base's document information dictionary, when present.
    pub info: Option<ObjRef>,
    /// The base trailer's `/ID` array, cloned.
    pub id: Option<Object>,
}

impl OverlayBase {
    /// Reads `doc`'s trailer and newest cross-reference section.
    pub fn from_document(doc: &Document) -> Result<OverlayBase> {
        let trailer = &doc.xref().trailer;
        if trailer.get("Encrypt").is_some() {
            return Err(Error::EncryptedBase);
        }
        let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
        let prev = startxref(doc.bytes()).ok_or(Error::MissingStartxref)?;
        let kind = match parse_section_at(doc.bytes(), prev)
            .map_err(core_error)?
            .kind
        {
            XrefKind::Table => XrefStyle::Table,
            XrefKind::Stream => XrefStyle::Stream,
        };
        let highest = doc.xref().iter().map(|(num, _)| num).max().unwrap_or(0);
        let declared = trailer.get_int("Size").unwrap_or(0).max(0) as u32;
        Ok(OverlayBase {
            prev: prev as u64,
            kind,
            size: declared.max(highest + 1),
            root,
            info: trailer.get_ref("Info"),
            id: trailer.get("ID").cloned(),
        })
    }
}

/// One recorded change against an object number: a new or replacement body
/// from [`Overlay::set`], or a free marker from [`Overlay::remove`].
#[derive(Debug, Clone)]
enum Change {
    Set(Object),
    Free,
}

/// An update section under construction over an [`OverlayBase`]: which
/// objects it holds, new ones numbered from the base's first free number.
#[derive(Debug, Clone)]
pub struct Overlay {
    base: OverlayBase,
    next: u32,
    objects: Vec<(ObjRef, Change)>,
    imported: FastMap<ObjRef, ObjRef>,
    info: Option<ObjRef>,
}

impl Overlay {
    /// An empty update section over `base`, numbering new objects from its
    /// first free number.
    pub fn new(base: OverlayBase) -> Overlay {
        let next = base.size;
        Overlay {
            base,
            next,
            objects: Vec::new(),
            imported: FastMap::default(),
            info: None,
        }
    }

    /// Sets an object under its own number, whether new or a replacement
    /// of one already in the base. Raises the next free number past `r`
    /// when `r` was not already reserved, so a later `reserve`/`put` never
    /// collides with a caller-chosen number.
    pub fn set(&mut self, r: ObjRef, obj: Object) {
        self.next = self.next.max(r.num.saturating_add(1));
        self.objects.push((r, Change::Set(obj)));
    }

    /// Marks `r` free: the appended section's cross-reference data chains
    /// it into entry 0's free list, in whichever style the base uses. Its
    /// generation for reuse is `r.gen` advanced by one (saturating at
    /// 65535, the field's own limit), per the classic table's convention
    /// for a deleted entry's row. A no-op for object number 0: it is
    /// already the free list's own permanent head, represented by this
    /// section's synthetic entry-0 row whenever any other object is freed.
    pub fn remove(&mut self, r: ObjRef) {
        if r.num == 0 {
            return;
        }
        self.next = self.next.max(r.num.saturating_add(1));
        let gen = r.gen.saturating_add(1);
        self.objects
            .push((ObjRef { num: r.num, gen }, Change::Free));
    }

    /// Allocates the next free object number without storing anything
    /// under it yet.
    pub fn reserve(&mut self) -> ObjRef {
        let r = ObjRef {
            num: self.next,
            gen: 0,
        };
        self.next += 1;
        r
    }

    /// Adds a new object under the next free number.
    pub fn put(&mut self, obj: Object) -> ObjRef {
        let r = self.reserve();
        self.set(r, obj);
        r
    }

    /// Registers the document information dictionary for the appended
    /// section's trailer, overriding the base's own.
    pub fn set_info(&mut self, r: ObjRef) {
        self.info = Some(r);
    }

    /// Whether nothing has been set or removed yet.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// The appended section alone: every set object at `start` plus its
    /// position within this section, then a cross-reference section in the
    /// base's style naming the base's section as `/Prev`. Refused when no
    /// object has been set. A number recorded more than once (repeated
    /// `set`, or `set` and `remove` on the same reference) keeps only its
    /// last-recorded change, so the appended cross-reference data never
    /// carries two rows for one number.
    pub fn section(&self, start: u64) -> Result<Vec<u8>> {
        if self.is_empty() {
            return Err(Error::EmptyUpdate);
        }
        let mut by_num: FastMap<u32, (ObjRef, Change)> = FastMap::default();
        for (r, change) in &self.objects {
            by_num.insert(r.num, (*r, change.clone()));
        }
        let mut changes: Vec<(ObjRef, Change)> = by_num.into_values().collect();
        changes.sort_by_key(|(r, _)| r.num);
        let mut out = Vec::new();
        let mut rows: Vec<Row> = Vec::with_capacity(changes.len() + 1);
        let mut freed: Vec<ObjRef> = Vec::new();
        for (r, change) in &changes {
            match change {
                Change::Set(obj) => {
                    rows.push(Row::InFile(*r, start as usize + out.len()));
                    write_indirect(&mut out, *r, obj)?;
                }
                Change::Free => freed.push(*r),
            }
        }
        if !freed.is_empty() {
            freed.sort_by_key(|r| r.num);
            let head = freed.first().map_or(0, |r| r.num);
            rows.push(Row::Free {
                num: 0,
                gen: 65535,
                next: head,
            });
            for (index, r) in freed.iter().enumerate() {
                let next = freed.get(index + 1).map_or(0, |n| n.num);
                rows.push(Row::Free {
                    num: r.num,
                    gen: r.gen,
                    next,
                });
            }
        }
        rows.sort_by_key(Row::num);

        let mut trailer = Dict::new();
        trailer.insert(name("Root"), Object::Ref(self.base.root));
        if let Some(info) = self.info.or(self.base.info) {
            trailer.insert(name("Info"), Object::Ref(info));
        }
        if let Some(id) = rotated_id(&self.base, &out) {
            trailer.insert(name("ID"), id);
        }
        trailer.insert(name("Prev"), Object::Int(self.base.prev as i64));
        match self.base.kind {
            XrefStyle::Stream => finish_stream(&mut out, start, rows, trailer, self.next)?,
            XrefStyle::Table => finish_table(&mut out, start, &rows, trailer, self.next)?,
        }
        Ok(out)
    }

    /// The overlay's first page as a form XObject in the base's object
    /// space: its media box as the form's bounding box, its decoded content
    /// as the form's stream, and its resource graph deep-copied and
    /// renumbered.
    pub(crate) fn import_form(&mut self, overlay: &Document) -> Result<ObjRef> {
        let page = overlay.page(0).map_err(core_error)?;
        let content = page.content(overlay).map_err(core_error)?;
        let resources = self.import_object(overlay, &Object::Dict(page.resources.clone()))?;
        let bbox = page.media_box;
        let mut dict = Dict::new();
        dict.insert(name("Type"), Object::Name(name("XObject")));
        dict.insert(name("Subtype"), Object::Name(name("Form")));
        dict.insert(name("FormType"), Object::Int(1));
        dict.insert(
            name("BBox"),
            Object::Array(
                [bbox.x0, bbox.y0, bbox.x1, bbox.y1]
                    .iter()
                    .map(|v| Object::Real(f64::from(*v)))
                    .collect(),
            ),
        );
        dict.insert(name("Resources"), resources);
        dict.insert(name("Filter"), Object::Name(name("FlateDecode")));
        Ok(self.put(Object::Stream(Stream {
            dict,
            data: deflate(&content),
        })))
    }

    /// A deep copy of `obj` from `source` into the update: every reference
    /// it reaches becomes a new object here, each source object copied once
    /// however many times it is referenced. Streams keep their encoded
    /// bytes and filters; their `/Length` is rewritten on emission.
    pub(crate) fn import_object(&mut self, source: &Document, obj: &Object) -> Result<Object> {
        Ok(match obj {
            Object::Ref(r) => {
                if let Some(copied) = self.imported.get(r) {
                    return Ok(Object::Ref(*copied));
                }
                let copied = self.reserve();
                self.imported.insert(*r, copied);
                let body = source.get(*r).map_err(core_error)?;
                let body = self.import_object(source, &body)?;
                self.objects.push((copied, Change::Set(body)));
                Object::Ref(copied)
            }
            Object::Dict(d) => Object::Dict(self.import_dict(source, d)?),
            Object::Array(items) => Object::Array(
                items
                    .iter()
                    .map(|item| self.import_object(source, item))
                    .collect::<Result<Vec<Object>>>()?,
            ),
            Object::Stream(s) => {
                let mut dict = s.dict.clone();
                dict.remove("Length");
                Object::Stream(Stream {
                    dict: self.import_dict(source, &dict)?,
                    data: s.data.clone(),
                })
            }
            other => other.clone(),
        })
    }

    pub(crate) fn import_dict(&mut self, source: &Document, dict: &Dict) -> Result<Dict> {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            out.insert(key.clone(), self.import_object(source, value)?);
        }
        Ok(out)
    }
}

/// The base's length as an update's write position, plus whether a pad
/// newline must be inserted first: an object header may not follow
/// directly after `%%EOF` unless the base already ends on a line
/// terminator (`\n` or `\r`).
pub fn start_offset(base: &[u8]) -> (u64, bool) {
    let pad = !matches!(base.last(), Some(b'\n') | Some(b'\r'));
    (base.len() as u64 + u64::from(pad), pad)
}

/// A base document plus the update section being built over it.
pub struct Update<'a> {
    doc: &'a Document,
    overlay: Overlay,
}

impl<'a> Update<'a> {
    /// Opens `doc` for an update: refuses an encrypted base or one missing
    /// `/Root` or a `startxref` to chain the appended section's `/Prev` to.
    pub fn new(doc: &'a Document) -> Result<Update<'a>> {
        let base = OverlayBase::from_document(doc)?;
        Ok(Update {
            doc,
            overlay: Overlay::new(base),
        })
    }

    /// Sets an object under its own number, whether new or a replacement
    /// of one already in the base.
    pub fn set(&mut self, r: ObjRef, obj: Object) {
        self.overlay.set(r, obj);
    }

    /// Marks `r` free in the appended section's cross-reference data.
    pub fn remove(&mut self, r: ObjRef) {
        self.overlay.remove(r);
    }

    /// Allocates the next free object number without storing anything
    /// under it yet.
    pub fn reserve(&mut self) -> ObjRef {
        self.overlay.reserve()
    }

    /// The update section under construction.
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// The base bytes, whether a pad newline goes before the appended
    /// section, and the section itself, computed together so a refused
    /// update (or any other failure) is known before anything is written
    /// anywhere.
    fn parts(&self) -> Result<(&[u8], bool, Vec<u8>)> {
        let base = self.doc.bytes();
        let (start, pad) = start_offset(base);
        let section = self.overlay.section(start)?;
        Ok((base, pad, section))
    }

    /// Writes the base bytes, a pad newline when the base needs one, and
    /// the appended section into `out`. The section is built before any
    /// byte reaches `out`, so a refused update (or any other failure)
    /// writes nothing at all.
    pub fn append_into(&self, mut out: impl std::io::Write) -> Result<()> {
        let (base, pad, section) = self.parts()?;
        out.write_all(base)?;
        if pad {
            out.write_all(b"\n")?;
        }
        out.write_all(&section)?;
        Ok(())
    }

    /// [`Update::append_into`] to a new file at `path`: the file is
    /// created only once the update is known to build, so a refused
    /// update leaves no file behind at all.
    pub fn save_appended(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let (base, pad, section) = self.parts()?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(base)?;
        if pad {
            file.write_all(b"\n")?;
        }
        file.write_all(&section)?;
        Ok(())
    }

    /// The base bytes followed by the update section, as one buffer.
    pub fn appended(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.append_into(&mut out)?;
        Ok(out)
    }
}

/// One row of the appended section's cross-reference data: an object
/// stored at a byte offset, or a freed number chained to the next free
/// number in the section's own free list (entry 0 when it is the head).
#[derive(Debug, Clone, Copy)]
enum Row {
    InFile(ObjRef, usize),
    Free { num: u32, gen: u16, next: u32 },
}

impl Row {
    fn num(&self) -> u32 {
        match self {
            Row::InFile(r, _) => r.num,
            Row::Free { num, .. } => *num,
        }
    }
}

/// Splits `rows`, already sorted ascending by object number, into maximal
/// runs of consecutive numbers, as `(run start index, run length)` pairs.
/// Shared by the classic table's subsections and the xref stream's
/// `/Index` pairs, so both group the same way.
fn contiguous_runs(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut begin = 0;
    while begin < rows.len() {
        let mut end = begin + 1;
        while end < rows.len() && rows[end].num() == rows[end - 1].num() + 1 {
            end += 1;
        }
        runs.push((begin, end - begin));
        begin = end;
    }
    runs
}

/// The appended trailer's `/ID`: the base's first half kept verbatim, the
/// second half replaced by the first 16 bytes of a SHA-256 over the first
/// half's bytes, the base's `/Prev` offset as little-endian bytes, and
/// `body` (the section's serialized objects, built before its xref part).
/// `None` when the base carries no `/ID` array with a string first
/// element, in which case the appended trailer omits the key entirely.
fn rotated_id(base: &OverlayBase, body: &[u8]) -> Option<Object> {
    let Some(Object::Array(halves)) = &base.id else {
        return None;
    };
    let Some(Object::String(first)) = halves.first() else {
        return None;
    };
    let mut hasher = Sha256::new();
    hasher.update(first);
    hasher.update(&base.prev.to_le_bytes());
    hasher.update(body);
    let digest = hasher.finalize();
    Some(Object::Array(vec![
        Object::String(first.clone()),
        Object::String(digest[..16].to_vec()),
    ]))
}

/// A cross-reference stream as the section's last object: one row per
/// object of the update (or per freed number) plus one for the stream
/// itself, and `/Index` pairs one per contiguous run of object numbers.
fn finish_stream(
    out: &mut Vec<u8>,
    start: u64,
    mut rows: Vec<Row>,
    mut dict: Dict,
    mut next: u32,
) -> Result<()> {
    let xref_ref = ObjRef { num: next, gen: 0 };
    next += 1;
    let xref_offset = start as usize + out.len();
    rows.push(Row::InFile(xref_ref, xref_offset));
    rows.sort_by_key(Row::num);
    let runs = contiguous_runs(&rows);
    let mut index = Vec::with_capacity(runs.len() * 2);
    for (begin, len) in runs {
        index.push(Object::Int(i64::from(rows[begin].num())));
        index.push(Object::Int(len as i64));
    }
    let mut data = Vec::with_capacity(rows.len() * 7);
    for row in &rows {
        match row {
            Row::InFile(r, offset) => {
                data.push(1);
                data.extend_from_slice(&field_offset(*offset)?.to_be_bytes());
                data.extend_from_slice(&r.gen.to_be_bytes());
            }
            Row::Free {
                gen,
                next: free_next,
                ..
            } => {
                data.push(0);
                data.extend_from_slice(&free_next.to_be_bytes());
                data.extend_from_slice(&gen.to_be_bytes());
            }
        }
    }
    dict.insert(name("Type"), Object::Name(name("XRef")));
    dict.insert(name("Size"), Object::Int(i64::from(next)));
    dict.insert(
        name("W"),
        Object::Array(vec![Object::Int(1), Object::Int(4), Object::Int(2)]),
    );
    dict.insert(name("Index"), Object::Array(index));
    write_indirect(out, xref_ref, &Object::Stream(Stream { dict, data }))?;
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
}

/// A classic `xref` table with one subsection per run of consecutive
/// object numbers, then the `trailer` dictionary. A freed row uses `f` in
/// place of `n`, its first field naming the next free number in the
/// section's own chain rather than a byte offset.
fn finish_table(
    out: &mut Vec<u8>,
    start: u64,
    rows: &[Row],
    mut dict: Dict,
    size: u32,
) -> Result<()> {
    let xref_offset = start as usize + out.len();
    out.extend_from_slice(b"xref\n");
    for (begin, len) in contiguous_runs(rows) {
        out.extend_from_slice(format!("{} {}\n", rows[begin].num(), len).as_bytes());
        for row in &rows[begin..begin + len] {
            match row {
                Row::InFile(r, offset) => out.extend_from_slice(
                    format!("{:010} {:05} n \n", table_offset(*offset)?, r.gen).as_bytes(),
                ),
                Row::Free { gen, next, .. } => {
                    out.extend_from_slice(format!("{next:010} {gen:05} f \n").as_bytes())
                }
            }
        }
    }
    dict.insert(name("Size"), Object::Int(i64::from(size)));
    out.extend_from_slice(b"trailer\n");
    serialize_dict(&dict, out)?;
    out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
}

/// Emits `num gen obj` through `endobj`; a stream carries a direct
/// `/Length` of its stored byte count.
fn write_indirect(out: &mut Vec<u8>, r: ObjRef, obj: &Object) -> Result<()> {
    out.extend_from_slice(format!("{} {} obj\n", r.num, r.gen).as_bytes());
    match obj {
        Object::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.insert(name("Length"), Object::Int(s.data.len() as i64));
            serialize_dict(&dict, out)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.data);
            out.extend_from_slice(b"\nendstream\nendobj\n");
        }
        direct => {
            serialize_object(direct, out)?;
            out.extend_from_slice(b"\nendobj\n");
        }
    }
    Ok(())
}

/// An uncompressed stream with no filter of its own.
fn plain_stream(data: Vec<u8>) -> Stream {
    Stream {
        dict: Dict::new(),
        data,
    }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("writing into a Vec cannot fail");
    encoder
        .finish()
        .expect("finishing an in-memory zlib stream cannot fail")
}

/// A byte position as the 4-byte offset field of a cross-reference stream.
fn field_offset(position: usize) -> Result<u32> {
    u32::try_from(position)
        .map_err(|_| Error::Other("file offset exceeds the 4-byte xref field".to_string()))
}

/// A byte position as the 10-digit offset field of a classic xref table.
fn table_offset(position: usize) -> Result<usize> {
    if position as u64 <= 9_999_999_999 {
        return Ok(position);
    }
    Err(Error::Other(
        "file offset exceeds the 10-digit xref table field".to_string(),
    ))
}

fn name(text: &str) -> Name {
    Name(text.to_string())
}

fn core_error(error: pdfboss_core::Error) -> Error {
    Error::Other(error.to_string())
}
