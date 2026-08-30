//! Incremental updates to an existing file (ISO 32000-1 §7.5.6): the base
//! bytes stay in place and an update section appends new and replaced
//! objects plus a cross-reference section chained to the base's by `/Prev`,
//! in the base's own cross-reference style.

use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdfboss_core::xref::startxref;
use pdfboss_core::{Dict, Document, FastMap, Name, ObjRef, Object, Stream};

use crate::error::{Error, Result};
use crate::ser::{serialize_dict, serialize_object};
use crate::writer::{WriteOptions, Writer};

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
        return Err(Error::Other(
            "an encrypted file cannot be rewritten".to_string(),
        ));
    }
    let root = trailer
        .get_ref("Root")
        .ok_or_else(|| Error::Other("the base file has no /Root".to_string()))?;
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
    let mut update = Update::open(base)?;
    let form = update.import_form(overlay)?;
    let prefix = update.put(Object::Stream(plain_stream(b"q\n".to_vec())));
    let suffix = update.put(Object::Stream(plain_stream(
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
        update.replace(page_ref, Object::Dict(dict));
    }
    update.finish()
}

/// One update section under construction: the objects it will hold, new
/// ones numbered from the first number the base leaves free.
struct Update<'a> {
    base: &'a Document,
    next: u32,
    objects: Vec<(ObjRef, Object)>,
    imported: FastMap<ObjRef, ObjRef>,
}

impl<'a> Update<'a> {
    fn open(base: &'a Document) -> Result<Update<'a>> {
        let trailer = &base.xref().trailer;
        if trailer.get("Encrypt").is_some() {
            return Err(Error::Other(
                "an encrypted file cannot be updated in place".to_string(),
            ));
        }
        let highest = base.xref().iter().map(|(num, _)| num).max().unwrap_or(0);
        let size = trailer.get_int("Size").unwrap_or(0).max(0) as u32;
        Ok(Update {
            base,
            next: size.max(highest + 1),
            objects: Vec::new(),
            imported: FastMap::default(),
        })
    }

    /// Adds a new object under the next free number.
    fn put(&mut self, obj: Object) -> ObjRef {
        let r = ObjRef {
            num: self.next,
            gen: 0,
        };
        self.next += 1;
        self.objects.push((r, obj));
        r
    }

    /// Replaces an object of the base under its own number.
    fn replace(&mut self, r: ObjRef, obj: Object) {
        self.objects.push((r, obj));
    }

    /// The overlay's first page as a form XObject in the base's object
    /// space: its media box as the form's bounding box, its decoded content
    /// as the form's stream, and its resource graph deep-copied and
    /// renumbered.
    fn import_form(&mut self, overlay: &Document) -> Result<ObjRef> {
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
    fn import_object(&mut self, source: &Document, obj: &Object) -> Result<Object> {
        Ok(match obj {
            Object::Ref(r) => {
                if let Some(copied) = self.imported.get(r) {
                    return Ok(Object::Ref(*copied));
                }
                let copied = ObjRef {
                    num: self.next,
                    gen: 0,
                };
                self.next += 1;
                self.imported.insert(*r, copied);
                let body = source.get(*r).map_err(core_error)?;
                let body = self.import_object(source, &body)?;
                self.objects.push((copied, body));
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

    fn import_dict(&mut self, source: &Document, dict: &Dict) -> Result<Dict> {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            out.insert(key.clone(), self.import_object(source, value)?);
        }
        Ok(out)
    }

    /// The base bytes followed by the update section: every object, then a
    /// cross-reference section in the base's style naming the base's
    /// section as `/Prev`.
    fn finish(mut self) -> Result<Vec<u8>> {
        let base_bytes = self.base.bytes();
        let prev = startxref(base_bytes)
            .ok_or_else(|| Error::Other("the base file has no startxref".to_string()))?;
        let mut out = base_bytes.to_vec();
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        self.objects.sort_by_key(|(r, _)| r.num);
        let mut rows: Vec<(ObjRef, usize)> = Vec::with_capacity(self.objects.len() + 1);
        for (r, obj) in &self.objects {
            rows.push((*r, out.len()));
            write_indirect(&mut out, *r, obj)?;
        }
        let trailer = &self.base.xref().trailer;
        let mut section = Dict::new();
        for key in ["Root", "Info", "ID"] {
            if let Some(value) = trailer.get(key) {
                section.insert(name(key), value.clone());
            }
        }
        section.insert(name("Prev"), Object::Int(prev as i64));
        let stream_style = trailer.get_name("Type").is_some_and(|n| n.0 == "XRef");
        if stream_style {
            self.finish_stream(&mut out, rows, section)?;
        } else {
            finish_table(&mut out, &rows, section, self.next)?;
        }
        Ok(out)
    }

    /// A cross-reference stream as the section's last object, rows for
    /// every object of the update and for the stream itself.
    fn finish_stream(
        &mut self,
        out: &mut Vec<u8>,
        mut rows: Vec<(ObjRef, usize)>,
        mut section: Dict,
    ) -> Result<()> {
        let xref_ref = ObjRef {
            num: self.next,
            gen: 0,
        };
        self.next += 1;
        let xref_offset = out.len();
        rows.push((xref_ref, xref_offset));
        let mut index = Vec::with_capacity(rows.len() * 2);
        let mut data = Vec::with_capacity(rows.len() * 7);
        for (r, offset) in &rows {
            index.push(Object::Int(i64::from(r.num)));
            index.push(Object::Int(1));
            data.push(1);
            data.extend_from_slice(&field_offset(*offset)?.to_be_bytes());
            data.extend_from_slice(&r.gen.to_be_bytes());
        }
        section.insert(name("Type"), Object::Name(name("XRef")));
        section.insert(name("Size"), Object::Int(i64::from(self.next)));
        section.insert(
            name("W"),
            Object::Array(vec![Object::Int(1), Object::Int(4), Object::Int(2)]),
        );
        section.insert(name("Index"), Object::Array(index));
        write_indirect(
            out,
            xref_ref,
            &Object::Stream(Stream {
                dict: section,
                data,
            }),
        )?;
        out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Ok(())
    }
}

/// A classic `xref` table with one subsection per run of consecutive
/// object numbers, then the `trailer` dictionary.
fn finish_table(
    out: &mut Vec<u8>,
    rows: &[(ObjRef, usize)],
    mut section: Dict,
    size: u32,
) -> Result<()> {
    let xref_offset = out.len();
    out.extend_from_slice(b"xref\n");
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && rows[end].0.num == rows[end - 1].0.num + 1 {
            end += 1;
        }
        out.extend_from_slice(format!("{} {}\n", rows[start].0.num, end - start).as_bytes());
        for (r, offset) in &rows[start..end] {
            out.extend_from_slice(
                format!("{:010} {:05} n \n", table_offset(*offset)?, r.gen).as_bytes(),
            );
        }
        start = end;
    }
    section.insert(name("Size"), Object::Int(i64::from(size)));
    out.extend_from_slice(b"trailer\n");
    serialize_dict(&section, out)?;
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
