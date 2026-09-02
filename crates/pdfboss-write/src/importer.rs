//! Transplanting a source document's object graph into a [`Writer`] (ISO
//! 32000 §7.3.10, indirect references): [`Importer`] renumbers every
//! reference it meets once and drains the resulting queue iteratively, so a
//! long chain of references costs no stack, and a caller may substitute a
//! translated body for any object before the drain reaches it.

use pdfboss_core::{Dict, Document, FastMap, Name, ObjRef, Object, Rect, Stream};

use crate::error::{Error, Result};
use crate::update::{core_error, deflate};
use crate::writer::Writer;

/// Copies one source document's reachable object graph into a [`Writer`].
/// Every reference met through [`Importer::reference`] or [`Importer::copy`]
/// is reserved a target number once and queued, so repeated imports from the
/// same source dedup by source object number: one `Importer` per source
/// document, and one `Writer` accepts several `Importer`s in sequence.
pub struct Importer<'w, 's> {
    writer: &'w mut Writer,
    source: &'s Document,
    map: FastMap<ObjRef, ObjRef>,
    pending: Vec<ObjRef>,
    substitutions: FastMap<ObjRef, Object>,
    compress: bool,
}

impl<'w, 's> Importer<'w, 's> {
    /// Opens `source` for import into `writer`. Refuses a locked source:
    /// `Document::get` only decrypts transparently once a working
    /// password configured a decryptor, so copying from a locked source
    /// would emit raw ciphertext into a plain target with no `/Encrypt`
    /// of its own. A password-opened encrypted document copies fine: its
    /// content already reads as plaintext through `Document::get`, exactly
    /// what [`crate::encrypt_document`] relies on to re-encrypt it under
    /// new passwords.
    pub fn new(writer: &'w mut Writer, source: &'s Document) -> Result<Importer<'w, 's>> {
        if source.is_locked() {
            return Err(Error::EncryptedBase);
        }
        let compress = writer.compress();
        Ok(Importer {
            writer,
            source,
            map: FastMap::default(),
            pending: Vec::new(),
            substitutions: FastMap::default(),
            compress,
        })
    }

    /// The target number for source reference `r`, reserved and queued on
    /// first sight.
    pub fn reference(&mut self, r: ObjRef) -> ObjRef {
        if let Some(copied) = self.map.get(&r) {
            return *copied;
        }
        let copied = self.writer.reserve();
        self.map.insert(r, copied);
        self.pending.push(r);
        copied
    }

    /// A copy of `obj` (source-space) with every reference renumbered into
    /// the target: the existing private translation, made public.
    pub fn copy(&mut self, obj: &Object) -> Result<Object> {
        Ok(match obj {
            Object::Ref(r) => Object::Ref(self.reference(*r)),
            Object::Dict(d) => Object::Dict(self.copy_dict(d)?),
            Object::Array(items) => Object::Array(
                items
                    .iter()
                    .map(|item| self.copy(item))
                    .collect::<Result<Vec<Object>>>()?,
            ),
            Object::Stream(_) => return Err(Error::NestedStream),
            other => other.clone(),
        })
    }

    /// A copy of `dict`'s direct structure with every reference mapped.
    pub(crate) fn copy_dict(&mut self, dict: &Dict) -> Result<Dict> {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            out.insert(key.clone(), self.copy(value)?);
        }
        Ok(out)
    }

    /// A stream body: its dictionary copied without `/Length` (the writer
    /// sets it), its data compressed when asked and not already filtered.
    fn copy_stream(&mut self, stream: &Stream) -> Result<Object> {
        let mut dict = stream.dict.clone();
        dict.remove("Length");
        let mut dict = self.copy_dict(&dict)?;
        let data = if self.compress && dict.get("Filter").is_none() {
            dict.insert(name("Filter"), Object::Name(name("FlateDecode")));
            deflate(&stream.data)
        } else {
            stream.data.clone()
        };
        Ok(Object::Stream(Stream { dict, data }))
    }

    /// Replaces the source object's body during the transplant. The
    /// body is TARGET-space and drain fills it verbatim, no
    /// renumbering: the caller translates any source refs into it via
    /// `reference`/`copy` first, and may use refs of objects already
    /// in the writer directly.
    ///
    /// Must be called before `finish` drains `r`: once `r` is popped off
    /// the pending queue and filled, a later `substitute` for it has no
    /// effect. `page` cannot trigger this hazard: it only substitutes a
    /// reference it has itself just queued for the first time.
    pub fn substitute(&mut self, r: ObjRef, body: Object) {
        self.substitutions.insert(r, body);
    }

    /// Drains the pending queue; called by `page`/`document` before
    /// returning, public for callers that mixed `reference` in.
    pub fn finish(&mut self) -> Result<()> {
        while let Some(r) = self.pending.pop() {
            let target = self.map[&r];
            let body = match self.substitutions.remove(&r) {
                Some(body) => body,
                None => match self.source.get(r).map_err(core_error)? {
                    Object::Stream(s) => self.copy_stream(&s)?,
                    other => self.copy(&other)?,
                },
            };
            self.writer.fill(target, body)?;
        }
        Ok(())
    }

    /// The whole reachable graph from the source catalog; returns the
    /// new root ref. Substitutions apply.
    pub fn document(&mut self) -> Result<ObjRef> {
        let root = self
            .source
            .xref()
            .trailer
            .get_ref("Root")
            .ok_or(Error::MissingRoot)?;
        let new_root = self.reference(root);
        self.finish()?;
        Ok(new_root)
    }

    /// Page `index` as a self-contained object under `parent`
    /// (target-space): old `/Parent` replaced with `parent`, effective
    /// `/Resources` and `/MediaBox` materialized, `/Rotate` when non-zero,
    /// `/CropBox` when it differs from the media box, `/Type /Page` always
    /// present. Returns the page's new ref. The source `/Parent` is dropped
    /// before translation, so none of the source's page tree (siblings,
    /// ancestors) rides along as unreachable objects in the target.
    ///
    /// ISO 32000 gives a page exactly one parent, so every call gets its
    /// own page object: importing the same source index again (for a
    /// second parent, or a repeat in an assembled document) never reuses
    /// an earlier call's object, even though the resources and content
    /// beneath keep deduping through this `Importer`'s map. The one
    /// exception is the very first time a given indirect source page is
    /// seen: that call keeps the page's source-graph identity (so an
    /// `/Annots` `/P` back-reference elsewhere in the graph resolves to
    /// this same object rather than a duplicate). Pages inlined into
    /// `/Kids` (no object of their own) always get a fresh object.
    pub fn page(&mut self, index: usize, parent: ObjRef) -> Result<ObjRef> {
        let page = self.source.page(index).map_err(core_error)?;
        let mut dict = page.dict().clone();
        dict.remove("Parent");
        dict.insert(name("Type"), Object::Name(name("Page")));
        dict.insert(name("Resources"), Object::Dict(page.resources.clone()));
        dict.insert(name("MediaBox"), rect_array(page.media_box));
        if page.rotate != 0 {
            dict.insert(name("Rotate"), Object::Int(i64::from(page.rotate)));
        }
        if page.crop_box != page.media_box {
            dict.insert(name("CropBox"), rect_array(page.crop_box));
        }
        let mut translated = self.copy_dict(&dict)?;
        translated.insert(name("Parent"), Object::Ref(parent));
        let target = match page.object_ref() {
            Some(r) if !self.map.contains_key(&r) => {
                let target = self.reference(r);
                self.substitute(r, Object::Dict(translated));
                target
            }
            _ => {
                let target = self.writer.reserve();
                self.writer.fill(target, Object::Dict(translated))?;
                target
            }
        };
        self.finish()?;
        Ok(target)
    }
}

/// A rectangle as a PDF `[x0 y0 x1 y1]` array of reals.
pub(crate) fn rect_array(rect: Rect) -> Object {
    Object::Array(
        [rect.x0, rect.y0, rect.x1, rect.y1]
            .iter()
            .map(|v| Object::Real(widen(*v)))
            .collect(),
    )
}

/// `value` widened to `f64` through its own shortest round-trip decimal,
/// rather than a raw bit widening: a plain `f64::from(f32)` keeps every
/// bit of the `f32`'s binary value, which usually needs far more decimal
/// digits to print than the `f32` itself means (`0.1_f32` widens to
/// `0.10000000149011612`, not `0.1`). Formatting the `f32` first and
/// reparsing produces the `f64` an `Object::Real` should hold: the one
/// whose decimal digits match what the source number meant.
fn widen(value: f32) -> f64 {
    value
        .to_string()
        .parse()
        .expect("a float's own Display output parses back as a float")
}

fn name(text: &str) -> Name {
    Name(text.to_string())
}

#[cfg(test)]
mod tests {
    use pdfboss_core::Rect;

    use super::*;
    use crate::ser::serialize_object;

    fn real_at(array: &Object, index: usize) -> String {
        let Object::Array(items) = array else {
            panic!("expected an array, got {array:?}");
        };
        let mut out = Vec::new();
        serialize_object(&items[index], &mut out).expect("a real serializes");
        String::from_utf8(out).expect("real syntax is ASCII")
    }

    #[test]
    fn rect_array_keeps_the_f32_shortest_decimal() {
        let rect = Rect::new(0.1, 0.0, 300.0, 400.0);
        let array = rect_array(rect);
        assert_eq!(real_at(&array, 0), "0.1");
    }
}
