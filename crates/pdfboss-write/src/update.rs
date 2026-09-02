//! Incremental updates to an existing file (ISO 32000-1 §7.5.6): the base
//! bytes stay in place and an update section appends new and replaced
//! objects plus a cross-reference section chained to the base's by `/Prev`,
//! in the base's own cross-reference style.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdfboss_core::crypt::Sha256;
use pdfboss_core::object::decode_text_string;
use pdfboss_core::xref::{parse_section_at, startxref};
use pdfboss_core::{Dict, Document, FastMap, Name, ObjRef, Object, Stream, XrefKind};

use crate::error::{Error, Result};
use crate::importer::{rect_array, Importer};
use crate::pdf::{text_string, Date, Metadata};
use crate::ser::{serialize_dict, serialize_object};
use crate::writer::{WriteOptions, Writer, XrefStyle};

/// The resource name every page draws the overlay form under.
const FORM_NAME: &str = "PdfbossWatermark";

/// Like [`watermark`], but writes a fresh file through the [`Writer`] under
/// `options` instead of appending an update: every object the base's
/// catalog reaches is copied over, uncompressed streams are compressed when
/// `options.compress` is set, and unreachable objects and earlier sections
/// are left behind, so the result is usually smaller than the base. Both
/// `base` and `overlay` are refused when encrypted, through
/// [`crate::importer::Importer::new`].
pub fn watermark_with(
    base: &Document,
    overlay: &Document,
    options: WriteOptions,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(options);
    let prefix = writer.put_stream_raw(Dict::new(), b"q\n".to_vec());
    let suffix = writer.put_stream_raw(
        Dict::new(),
        format!("Q\nq /{FORM_NAME} Do Q\n").into_bytes(),
    );
    let form = overlay_form(&mut writer, overlay)?;

    let trailer = &base.xref().trailer;
    let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
    let mut importer = Importer::new(&mut writer, base)?;
    let new_root = importer.reference(root);
    let new_info = trailer.get_ref("Info").map(|info| importer.reference(info));
    for index in 0..base.page_count() {
        let page = base.page(index).map_err(core_error)?;
        let Some(page_ref) = page.object_ref() else {
            continue;
        };
        let dict = marked_page_dict(&mut importer, base, index, form, prefix, suffix)?;
        importer.substitute(page_ref, dict);
    }
    importer.finish()?;
    if let Some(new_info) = new_info {
        writer.set_info(new_info);
    }
    writer.finish(new_root)
}

/// The marked dictionary for source page `index`: its own dictionary
/// translated into the target, its effective resources gaining the overlay
/// form under [`FORM_NAME`], and its content wrapped in `prefix` and
/// `suffix`.
fn marked_page_dict(
    importer: &mut Importer,
    base: &Document,
    index: usize,
    form: ObjRef,
    prefix: ObjRef,
    suffix: ObjRef,
) -> Result<Object> {
    let page = base.page(index).map_err(core_error)?;
    let mut dict = importer.copy_dict(page.dict())?;
    let mut resources = importer.copy_dict(&page.resources)?;
    let mut xobjects = match page.resources.get("XObject") {
        Some(existing) => {
            let existing = base.resolve(existing).map_err(core_error)?;
            match existing.as_dict() {
                Some(d) => importer.copy_dict(d)?,
                None => Dict::new(),
            }
        }
        None => Dict::new(),
    };
    xobjects.insert(name(FORM_NAME), Object::Ref(form));
    resources.insert(name("XObject"), Object::Dict(xobjects));
    dict.insert(name("Resources"), Object::Dict(resources));
    let mut contents = vec![Object::Ref(prefix)];
    match page.dict().get("Contents") {
        Some(Object::Array(items)) => {
            for item in items {
                contents.push(importer.copy(item)?);
            }
        }
        Some(Object::Ref(r)) => match base.get(*r).map_err(core_error)? {
            Object::Array(items) => {
                for item in &items {
                    contents.push(importer.copy(item)?);
                }
            }
            _ => contents.push(Object::Ref(importer.reference(*r))),
        },
        _ => {}
    }
    contents.push(Object::Ref(suffix));
    dict.insert(name("Contents"), Object::Array(contents));
    Ok(Object::Dict(dict))
}

/// The overlay's first page as a form XObject, filled directly into
/// `writer`: its media box as the bounding box, its decoded content
/// deflated, its resources imported from `overlay`.
fn overlay_form(writer: &mut Writer, overlay: &Document) -> Result<ObjRef> {
    let page = overlay.page(0).map_err(core_error)?;
    let content = page.content(overlay).map_err(core_error)?;
    let resources = {
        let mut importer = Importer::new(writer, overlay)?;
        let resources = importer.copy_dict(&page.resources)?;
        importer.finish()?;
        resources
    };
    let mut dict = Dict::new();
    dict.insert(name("Type"), Object::Name(name("XObject")));
    dict.insert(name("Subtype"), Object::Name(name("Form")));
    dict.insert(name("FormType"), Object::Int(1));
    dict.insert(name("BBox"), rect_array(page.media_box));
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

/// Draws the first page of `overlay` over every page of `base`, returning
/// `base`'s bytes followed by an incremental update: the overlay page as
/// one form XObject (its resources copied into the base's object space),
/// and each page's dictionary rewritten with that form in its resources
/// and its content wrapped in `q … Q` before the form is drawn. Pages
/// inlined directly into `/Kids`, having no object of their own, are left
/// as they are. An encrypted `base` is refused: its new strings and
/// streams would need encrypting too. An encrypted `overlay` is refused
/// as well: its decrypted content would otherwise copy across into the
/// plain update section.
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
    update.bytes()
}

/// Stages `by` degrees of rotation, clockwise, on each of `pages` (0-based
/// indices) into `update`: a clone of the page's own leaf dictionary, its
/// `/Rotate` set to its current effective rotation plus `by`, normalized
/// with `rem_euclid(360)`. The staged dictionary is untranslated: it
/// keeps its own `/Parent`, so it stays exactly where it was in the page
/// tree. A page with no object of its own (inlined directly into
/// `/Kids`) cannot be staged this way: refused, naming its 1-based page
/// number and pointing at `--rewrite`.
pub fn rotate_pages(update: &mut Update, pages: &[usize], by: i32) -> Result<()> {
    for &index in pages {
        let page = update.doc.page(index).map_err(core_error)?;
        let Some(page_ref) = page.object_ref() else {
            return Err(Error::Other(format!(
                "page {} has no object of its own (inlined into /Kids); use --rewrite instead",
                index + 1
            )));
        };
        let mut dict = page.dict().clone();
        let rotate = (page.rotate + by).rem_euclid(360);
        dict.insert(name("Rotate"), Object::Int(i64::from(rotate)));
        update.set(page_ref, Object::Dict(dict));
    }
    Ok(())
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
    /// Reads `doc`'s trailer and newest cross-reference section: the
    /// section's offset and kind come from `doc.xref().newest_section()`,
    /// already recorded while core loaded the file, falling back to
    /// re-deriving them from `startxref` and `parse_section_at` when the
    /// document has none (a recovery-scan base refuses with
    /// [`Error::MissingStartxref`] on this fallback path).
    pub fn from_document(doc: &Document) -> Result<OverlayBase> {
        let trailer = &doc.xref().trailer;
        if trailer.get("Encrypt").is_some_and(|o| !o.is_null()) {
            return Err(Error::EncryptedBase);
        }
        let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
        let (prev, kind) = match doc.xref().newest_section() {
            Some(section) => (section.offset, xref_style(section.kind)),
            None => {
                let offset = startxref(doc.bytes()).ok_or(Error::MissingStartxref)?;
                let kind = xref_style(
                    parse_section_at(doc.bytes(), offset)
                        .map_err(core_error)?
                        .kind,
                );
                (offset as u64, kind)
            }
        };
        let highest = doc.xref().iter().map(|(num, _)| num).max().unwrap_or(0);
        let declared = trailer.get_int("Size").unwrap_or(0).max(0) as u32;
        Ok(OverlayBase {
            prev,
            kind,
            size: declared.max(highest + 1),
            root,
            info: trailer.get_ref("Info"),
            id: trailer.get("ID").cloned(),
        })
    }
}

/// The [`XrefStyle`] an appended section should copy for a base whose
/// newest section is `kind`.
fn xref_style(kind: XrefKind) -> XrefStyle {
    match kind {
        XrefKind::Table => XrefStyle::Table,
        XrefKind::Stream => XrefStyle::Stream,
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
    /// collides with a caller-chosen number. A no-op for object number 0:
    /// it is already the free list's own permanent head, represented by
    /// this section's synthetic entry-0 row whenever any other object is
    /// freed, and a `set` row for it would collide with that row. Symmetric
    /// with [`Overlay::remove`]'s guard.
    pub fn set(&mut self, r: ObjRef, obj: Object) {
        if r.num == 0 {
            return;
        }
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
        let mut last: FastMap<u32, usize> = FastMap::default();
        for (index, (r, _)) in self.objects.iter().enumerate() {
            last.insert(r.num, index);
        }
        let mut winners: Vec<usize> = last.into_values().collect();
        winners.sort_by_key(|&index| self.objects[index].0.num);
        let mut out = Vec::new();
        let mut rows: Vec<Row> = Vec::with_capacity(winners.len() + 1);
        let mut freed: Vec<ObjRef> = Vec::new();
        for index in winners {
            let (r, change) = &self.objects[index];
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
        if let Some(id) = rotated_id(&self.base, &out, &freed) {
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
    /// renumbered. Refuses an encrypted `overlay`, for the same reason
    /// [`crate::importer::Importer::new`] refuses an encrypted source:
    /// copying its decrypted content across would silently strip its
    /// protection.
    pub(crate) fn import_form(&mut self, overlay: &Document) -> Result<ObjRef> {
        if overlay
            .xref()
            .trailer
            .get("Encrypt")
            .is_some_and(|o| !o.is_null())
        {
            return Err(Error::EncryptedBase);
        }
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

/// Merges `meta` into `existing_info`'s dictionary (or a fresh one, staged
/// under a newly reserved number, when `existing_info` is `None`): a
/// `Some` field overwrites its key (`Some(String::new())` writes an empty
/// string), a `None` field leaves whatever key was already there. The
/// merged dictionary is staged into `overlay` via `set` and `set_info`.
///
/// When `xmp_ref` is `Some`, the merged dictionary is read back into a
/// [`Metadata`] (text fields via `decode_text_string`, dates via
/// [`Date::parse_pdf`], an unparseable date simply dropping out) and
/// staged under `xmp_ref` as a fresh, unfiltered `/Type /Metadata /Subtype
/// /XML` stream of the crate's XMP packet over that merged value: any XMP
/// property outside those eight fields is not carried into the new
/// packet, though the original packet's bytes stay in the base.
///
/// Shared by [`Update::set_metadata`] and its asynchronous counterpart.
pub fn set_metadata_with(
    overlay: &mut Overlay,
    existing_info: Option<(ObjRef, Dict)>,
    xmp_ref: Option<ObjRef>,
    meta: Metadata,
) -> Result<()> {
    let (target, mut dict) = match existing_info {
        Some((r, dict)) => (r, dict),
        None => (overlay.reserve(), Dict::new()),
    };
    apply_metadata_fields(&mut dict, &meta);
    let merged = metadata_from_info(&dict);
    overlay.set(target, Object::Dict(dict));
    overlay.set_info(target);
    let Some(xmp_ref) = xmp_ref else {
        return Ok(());
    };
    let mut xmp_dict = Dict::new();
    xmp_dict.insert(name("Type"), Object::Name(name("Metadata")));
    xmp_dict.insert(name("Subtype"), Object::Name(name("XML")));
    overlay.set(
        xmp_ref,
        Object::Stream(Stream {
            dict: xmp_dict,
            data: crate::xmp::packet(&merged),
        }),
    );
    Ok(())
}

/// Writes every `Some` field of `meta` into `dict` under its `/Info` key;
/// a `None` field is left untouched.
fn apply_metadata_fields(dict: &mut Dict, meta: &Metadata) {
    let texts = [
        ("Title", &meta.title),
        ("Author", &meta.author),
        ("Subject", &meta.subject),
        ("Keywords", &meta.keywords),
        ("Creator", &meta.creator),
        ("Producer", &meta.producer),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            dict.insert(name(key), text_string(value));
        }
    }
    let dates = [
        ("CreationDate", meta.creation_date),
        ("ModDate", meta.modification_date),
    ];
    for (key, value) in dates {
        if let Some(date) = value {
            dict.insert(name(key), Object::String(date.to_pdf_string().into_bytes()));
        }
    }
}

/// Reads an `/Info` dictionary back into a [`Metadata`]: text fields via
/// `decode_text_string`, dates via [`Date::parse_pdf`]. A missing or
/// unparseable field is simply `None`.
fn metadata_from_info(dict: &Dict) -> Metadata {
    Metadata {
        title: info_text(dict, "Title"),
        author: info_text(dict, "Author"),
        subject: info_text(dict, "Subject"),
        keywords: info_text(dict, "Keywords"),
        creator: info_text(dict, "Creator"),
        producer: info_text(dict, "Producer"),
        creation_date: info_date(dict, "CreationDate"),
        modification_date: info_date(dict, "ModDate"),
    }
}

/// `dict[key]` decoded as a text string, when present and a string.
fn info_text(dict: &Dict, key: &str) -> Option<String> {
    Some(decode_text_string(dict.get(key)?.as_str_bytes()?))
}

/// `dict[key]` decoded and parsed as a PDF date, when present, a string,
/// and a valid date.
fn info_date(dict: &Dict, key: &str) -> Option<Date> {
    Date::parse_pdf(&decode_text_string(dict.get(key)?.as_str_bytes()?))
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

    /// Merges `meta` into the base document's `/Info` dictionary: the ref
    /// comes from the overlay's own info ref when a prior call set one,
    /// else the base's; the dictionary itself always comes from the base
    /// document (never from a prior call's staged fields, so two calls on
    /// one `Update` do not compound; on a base without `/Info`, a call
    /// starts from a fresh dictionary). When the catalog already names an
    /// XMP packet, it is rewritten from the merged fields. See
    /// [`set_metadata_with`] for the merge and rewrite rules.
    pub fn set_metadata(&mut self, meta: Metadata) -> Result<()> {
        let info_ref = self.overlay.info.or(self.overlay.base.info);
        let existing_info = info_ref.and_then(|r| {
            let dict = self.doc.get(r).ok()?.as_dict()?.clone();
            Some((r, self.resolve_dict(&dict)))
        });
        let xmp_ref = self.catalog_metadata_ref();
        set_metadata_with(&mut self.overlay, existing_info, xmp_ref, meta)
    }

    /// `dict` with every value resolved against the base document: an
    /// indirect value such as `/Title 12 0 R` becomes the string object it
    /// points to, so a field kept by [`set_metadata_with`] (a `None` field
    /// in the merge) still reads back as text rather than silently
    /// vanishing from the rewritten XMP packet. A value whose reference
    /// chain fails to resolve (an unreadable target, or a cycle) is kept
    /// as given.
    fn resolve_dict(&self, dict: &Dict) -> Dict {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            let resolved = self.doc.resolve(value).unwrap_or_else(|_| value.clone());
            out.insert(key.clone(), resolved);
        }
        out
    }

    /// The catalog's `/Metadata` entry, when it is an indirect reference.
    /// `None` for a catalog with no `/Metadata`, or one that reads as a
    /// direct stream rather than a reference.
    fn catalog_metadata_ref(&self) -> Option<ObjRef> {
        let catalog = self.doc.get(self.overlay.base.root).ok()?;
        match catalog.as_dict()?.get("Metadata")? {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
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
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
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
    pub fn bytes(&self) -> Result<Vec<u8>> {
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
/// half's bytes, the base's `/Prev` offset as little-endian bytes, `body`
/// (the section's serialized objects, built before its xref part), and
/// finally each of `freed`'s `(num, gen)` pairs in order (`num` then `gen`,
/// both little-endian), so a frees-only update, whose `body` is empty,
/// still rotates by what it freed rather than staying fixed. `None` when
/// the base carries no `/ID` array with a string first element, in which
/// case the appended trailer omits the key entirely.
fn rotated_id(base: &OverlayBase, body: &[u8], freed: &[ObjRef]) -> Option<Object> {
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
    for r in freed {
        hasher.update(&r.num.to_le_bytes());
        hasher.update(&r.gen.to_le_bytes());
    }
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

pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
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

pub(crate) fn core_error(error: pdfboss_core::Error) -> Error {
    Error::Other(error.to_string())
}
