//! Document assembly: pages of canvas content, document metadata, and the
//! save path. `Pdf` is a plain struct — the fields are the composition,
//! and `Default` fills everything optional.

use std::path::Path;

use pdfboss_core::{Dict, Name, ObjRef, Object};

use crate::canvas::Canvas;
use crate::content::serialize_ops;
use crate::error::{Error, Result};
use crate::font::Standard14;
use crate::writer::{WriteOptions, Writer};

/// A page size, in default user-space units (1/72 inch), portrait.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PageSize {
    /// 297 × 420 mm.
    A3,
    /// 210 × 297 mm.
    #[default]
    A4,
    /// 148 × 210 mm.
    A5,
    /// 8.5 × 11 in.
    Letter,
    /// 8.5 × 14 in.
    Legal,
    /// Explicit dimensions in user-space units.
    Custom {
        /// Width in units.
        width: f32,
        /// Height in units.
        height: f32,
    },
}

impl PageSize {
    /// Width and height in user-space units.
    pub fn dimensions(self) -> (f32, f32) {
        match self {
            PageSize::A3 => (841.89, 1190.55),
            PageSize::A4 => (595.28, 841.89),
            PageSize::A5 => (419.53, 595.28),
            PageSize::Letter => (612.0, 792.0),
            PageSize::Legal => (612.0, 1008.0),
            PageSize::Custom { width, height } => (width, height),
        }
    }

    /// The same size with width and height swapped.
    pub fn landscape(self) -> PageSize {
        let (width, height) = self.dimensions();
        PageSize::Custom {
            width: height,
            height: width,
        }
    }
}

/// A calendar date and time with a UTC offset, for `/CreationDate` and
/// `/ModDate`. The writer never reads a clock — dates appear in output
/// only when a caller provides them, keeping builds reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// Four-digit year.
    pub year: u16,
    /// Month, 1–12.
    pub month: u8,
    /// Day of month, 1–31.
    pub day: u8,
    /// Hour, 0–23.
    pub hour: u8,
    /// Minute, 0–59.
    pub minute: u8,
    /// Second, 0–59.
    pub second: u8,
    /// Offset from UTC in minutes (positive east).
    pub utc_offset_minutes: i16,
}

impl Date {
    /// Formats as a PDF date string, `D:YYYYMMDDHHmmSSOHH'mm` — with a
    /// literal `Z` in place of the offset when the date is exactly UTC.
    pub fn to_pdf_string(self) -> String {
        let Date {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes,
        } = self;
        let mut out = format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}");
        if utc_offset_minutes == 0 {
            out.push('Z');
            return out;
        }
        let sign = if utc_offset_minutes < 0 { '-' } else { '+' };
        let magnitude = utc_offset_minutes.unsigned_abs();
        out.push_str(&format!(
            "{sign}{:02}'{:02}",
            magnitude / 60,
            magnitude % 60
        ));
        out
    }
}

/// Document information written to the `/Info` dictionary. Every field is
/// optional; an all-`None` value writes no dictionary at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// `/Title`.
    pub title: Option<String>,
    /// `/Author`.
    pub author: Option<String>,
    /// `/Subject`.
    pub subject: Option<String>,
    /// `/Keywords`.
    pub keywords: Option<String>,
    /// `/Creator` (the producing application's name).
    pub creator: Option<String>,
    /// `/Producer`.
    pub producer: Option<String>,
    /// `/CreationDate`.
    pub creation_date: Option<Date>,
    /// `/ModDate`.
    pub modification_date: Option<Date>,
}

/// One page: its size, rotation and painted content.
#[derive(Debug, Default)]
pub struct Page {
    /// Page size (the `/MediaBox`).
    pub size: PageSize,
    /// Clockwise view rotation in degrees; must be a multiple of 90.
    pub rotation: i32,
    /// The page's painted content.
    pub canvas: Canvas,
}

impl Page {
    /// An empty page of the given size.
    pub fn new(size: PageSize) -> Page {
        Page {
            size,
            ..Page::default()
        }
    }
}

/// A document under construction. The fields are the composition:
/// singleton slots are `Option`s, pages keep the order given.
#[derive(Debug, Default)]
pub struct Pdf {
    /// Document information, if any.
    pub metadata: Option<Metadata>,
    /// Pages, in reading order.
    pub pages: Vec<Page>,
    /// File-emission options.
    pub options: WriteOptions,
}

impl Pdf {
    /// Serializes the document to complete PDF file bytes.
    ///
    /// Fonts are shared document-wide: each distinct [`Standard14`] face
    /// gets one font object, in first-use order. Images are embedded per
    /// page with no cross-page deduplication — the same raster drawn on
    /// two pages is stored twice.
    pub fn to_bytes(self) -> Result<Vec<u8>> {
        let Pdf {
            metadata,
            pages,
            options,
        } = self;
        let mut w = Writer::new(options);
        let pages_root = w.reserve();
        let mut font_cache: Vec<(Standard14, ObjRef)> = Vec::new();
        let mut kids = Vec::with_capacity(pages.len());
        for page in pages {
            if page.rotation % 90 != 0 {
                return Err(Error::Other(format!(
                    "page rotation {} is not a multiple of 90",
                    page.rotation
                )));
            }
            let (width, height) = page.size.dimensions();
            let rotation = page.rotation;
            let parts = page.canvas.into_parts();
            let content = w.put_stream(Dict::new(), serialize_ops(&parts.ops));
            let mut fonts = Dict::new();
            for (index, face) in parts.fonts.iter().enumerate() {
                let cached = font_cache
                    .iter()
                    .find(|(seen, _)| seen == face)
                    .map(|(_, r)| *r);
                let font_ref = match cached {
                    Some(r) => r,
                    None => {
                        let r = w.put(Object::Dict(face.font_dict()));
                        font_cache.push((*face, r));
                        r
                    }
                };
                fonts.insert(Name(format!("F{}", index + 1)), Object::Ref(font_ref));
            }
            let mut xobjects = Dict::new();
            for (index, image) in parts.images.iter().enumerate() {
                let image_ref = image.build_xobject(&mut w);
                xobjects.insert(Name(format!("Im{}", index + 1)), Object::Ref(image_ref));
            }
            let mut resources = Dict::new();
            if !fonts.is_empty() {
                resources.insert(name("Font"), Object::Dict(fonts));
            }
            if !xobjects.is_empty() {
                resources.insert(name("XObject"), Object::Dict(xobjects));
            }
            let mut dict = Dict::new();
            dict.insert(name("Type"), Object::Name(name("Page")));
            dict.insert(name("Parent"), Object::Ref(pages_root));
            dict.insert(
                name("MediaBox"),
                Object::Array(vec![
                    Object::Int(0),
                    Object::Int(0),
                    Object::Real(f64::from(width)),
                    Object::Real(f64::from(height)),
                ]),
            );
            dict.insert(name("Contents"), Object::Ref(content));
            dict.insert(name("Resources"), Object::Dict(resources));
            if rotation != 0 {
                dict.insert(name("Rotate"), Object::Int(i64::from(rotation)));
            }
            kids.push(Object::Ref(w.put(Object::Dict(dict))));
        }
        let mut tree = Dict::new();
        tree.insert(name("Type"), Object::Name(name("Pages")));
        tree.insert(name("Count"), Object::Int(kids.len() as i64));
        tree.insert(name("Kids"), Object::Array(kids));
        w.fill(pages_root, Object::Dict(tree))?;
        if let Some(info) = metadata.and_then(info_dict) {
            let info_ref = w.put(Object::Dict(info));
            w.set_info(info_ref);
        }
        let mut catalog = Dict::new();
        catalog.insert(name("Type"), Object::Name(name("Catalog")));
        catalog.insert(name("Pages"), Object::Ref(pages_root));
        let root = w.put(Object::Dict(catalog));
        w.finish(root)
    }

    /// Serializes and writes the document to `path`.
    pub fn save(self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let bytes = self.to_bytes()?;
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

/// A `Name` from a string literal.
fn name(text: &str) -> Name {
    Name(text.to_string())
}

/// Builds the `/Info` dictionary, or `None` when every field is `None`.
fn info_dict(meta: Metadata) -> Option<Dict> {
    let mut dict = Dict::new();
    let texts = [
        ("Title", meta.title),
        ("Author", meta.author),
        ("Subject", meta.subject),
        ("Keywords", meta.keywords),
        ("Creator", meta.creator),
        ("Producer", meta.producer),
    ];
    for (key, value) in texts {
        if let Some(value) = value {
            dict.insert(name(key), text_string(&value));
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
    if dict.is_empty() {
        return None;
    }
    Some(dict)
}

/// Encodes a text string (ISO 32000 §7.9.2.2): pure ASCII passes through
/// as its own bytes, anything else becomes UTF-16BE with a `FE FF` byte
/// order mark.
fn text_string(value: &str) -> Object {
    if value.is_ascii() {
        return Object::String(value.as_bytes().to_vec());
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_match_the_contract() {
        assert_eq!(PageSize::A3.dimensions(), (841.89, 1190.55));
        assert_eq!(PageSize::A4.dimensions(), (595.28, 841.89));
        assert_eq!(PageSize::A5.dimensions(), (419.53, 595.28));
        assert_eq!(PageSize::Letter.dimensions(), (612.0, 792.0));
        assert_eq!(PageSize::Legal.dimensions(), (612.0, 1008.0));
        assert_eq!(
            PageSize::Custom {
                width: 10.0,
                height: 20.0
            }
            .dimensions(),
            (10.0, 20.0)
        );
    }

    #[test]
    fn landscape_swaps_into_custom() {
        assert_eq!(
            PageSize::A4.landscape(),
            PageSize::Custom {
                width: 841.89,
                height: 595.28
            }
        );
        assert_eq!(
            PageSize::Custom {
                width: 1.0,
                height: 2.0
            }
            .landscape(),
            PageSize::Custom {
                width: 2.0,
                height: 1.0
            }
        );
        assert_eq!(PageSize::Letter.landscape().dimensions(), (792.0, 612.0));
    }

    #[test]
    fn date_utc_formats_with_z() {
        let date = Date {
            year: 2026,
            month: 8,
            day: 27,
            hour: 12,
            minute: 30,
            second: 15,
            utc_offset_minutes: 0,
        };
        assert_eq!(date.to_pdf_string(), "D:20260827123015Z");
    }

    #[test]
    fn date_positive_offset_pads_single_digits() {
        let date = Date {
            year: 987,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            second: 5,
            utc_offset_minutes: 120,
        };
        assert_eq!(date.to_pdf_string(), "D:09870102030405+02'00");
    }

    #[test]
    fn date_negative_offset_keeps_minutes() {
        let date = Date {
            year: 1999,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 58,
            utc_offset_minutes: -330,
        };
        assert_eq!(date.to_pdf_string(), "D:19991231235958-05'30");
    }
}
