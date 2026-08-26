//! Document assembly: pages of canvas content, document metadata, and the
//! save path. `Pdf` is a plain struct — the fields are the composition,
//! and `Default` fills everything optional.

use std::path::Path;

use crate::canvas::Canvas;
use crate::error::Result;
use crate::writer::WriteOptions;

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
        todo!("dimensions of {self:?}")
    }

    /// The same size with width and height swapped.
    pub fn landscape(self) -> PageSize {
        todo!("landscape of {self:?}")
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
    /// Formats as a PDF date string, `D:YYYYMMDDHHmmSSOHH'mm`.
    pub fn to_pdf_string(self) -> String {
        todo!("format {self:?}")
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
    pub fn to_bytes(self) -> Result<Vec<u8>> {
        todo!("assemble {} pages", self.pages.len())
    }

    /// Serializes and writes the document to `path`.
    pub fn save(self, path: impl AsRef<Path>) -> Result<()> {
        let unused = path.as_ref();
        todo!("save to {unused:?}")
    }
}
