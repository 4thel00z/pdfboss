//! The linearization parameter dictionary (ISO 32000-1 Annex F.3): the
//! first object of a linearized file, read as data. Nothing here uses the
//! hint streams or the first-page section to load pages early.

use crate::object::Dict;
use crate::parser::{NoResolve, Parser};

/// The leading bytes a linearization parameter dictionary must lie within
/// (ISO 32000-1 Annex F.3.3), so a reader decides whether a file is
/// linearized from one small read.
pub const LINEARIZATION_WINDOW: usize = 1024;

/// The linearization parameter dictionary (ISO 32000-1 Annex F.3, Table
/// F.1): the first object of a linearized file, whose entries locate the
/// first page and the hint streams so a reader can show the first page
/// before the whole file has arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Linearization {
    /// `/Linearized`: the version of the linearized format.
    pub version: f64,
    /// `/L`: the length of the file the dictionary was written for. When
    /// it differs from the actual length the file is ordinary PDF; see
    /// [`Linearization::is_current`].
    pub file_length: u64,
    /// `/H`: the offset and length of the primary hint stream and, when
    /// present, of the overflow hint stream, as written.
    pub hint_streams: Vec<(u64, u64)>,
    /// `/O`: the object number of the first page's page object.
    pub first_page_object: u32,
    /// `/E`: the offset of the end of the first page.
    pub first_page_end: u64,
    /// `/N`: the number of pages in the document.
    pub page_count: u32,
    /// `/T`: the offset of the main cross-reference table's first entry
    /// (of the main cross-reference stream in a file that uses streams).
    pub main_xref_offset: u64,
    /// `/P`: the page number of the first page, 0 when absent.
    pub first_page: u32,
}

impl Linearization {
    /// Whether `/L` equals the file's actual `file_length`. Table F.1 makes
    /// a mismatch mean the file is not linearized and shall be read as
    /// ordinary PDF, the usual cause being an appended update.
    ///
    /// Covers ISO 32000-1 Annex F.3.
    pub fn is_current(&self, file_length: u64) -> bool {
        self.file_length == file_length
    }
}

/// Reads the linearization parameter dictionary from `head`, the first
/// bytes of a file: the first indirect object of the body, which the annex
/// requires to lie entirely within the first 1024 bytes. `None` when that
/// object is not a dictionary carrying `/Linearized` or lacks one of Table
/// F.1's required entries, so a file whose first object is anything else
/// is simply not linearized.
///
/// Covers ISO 32000-1 Annex F.3.
pub fn linearization_dictionary(head: &[u8]) -> Option<Linearization> {
    let window = &head[..head.len().min(LINEARIZATION_WINDOW)];
    let (_, object) = Parser::new(window).parse_indirect(&NoResolve).ok()?;
    let dict = object.as_dict()?;
    let version = dict.get("Linearized")?.as_f64()?;
    let offset = |key: &str| u64::try_from(dict.get_int(key)?).ok();
    let count = |key: &str| u32::try_from(dict.get_int(key)?).ok();
    Some(Linearization {
        version,
        file_length: offset("L")?,
        hint_streams: hint_streams(dict)?,
        first_page_object: count("O")?,
        first_page_end: offset("E")?,
        page_count: count("N")?,
        main_xref_offset: offset("T")?,
        first_page: match dict.get_int("P") {
            Some(page) => u32::try_from(page).ok()?,
            None => 0,
        },
    })
}

/// The `/H` array as (offset, length) pairs: two or four integers, so an
/// odd count or a non-integer reads as a malformed dictionary.
fn hint_streams(dict: &Dict) -> Option<Vec<(u64, u64)>> {
    let values: Vec<u64> = dict
        .get_array("H")?
        .iter()
        .map(|value| u64::try_from(value.as_int()?).ok())
        .collect::<Option<_>>()?;
    let (pairs, rest) = values.as_chunks::<2>();
    if pairs.is_empty() || !rest.is_empty() {
        return None;
    }
    Some(
        pairs
            .iter()
            .map(|&[offset, length]| (offset, length))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    const HEAD: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n43 0 obj\n<< /Linearized 1 /L 12345 /H [ 500 200 ] /O 45 /E 3000 /N 3 /T 11000 /P 2 >>\nendobj\n44 0 obj\n<< /Type /Catalog >>\nendobj\n";

    /// A one-page file whose first object is a linearization parameter
    /// dictionary declaring `length` as the file length.
    fn linearized_file(length: &str) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.object(
            43,
            &format!("<< /Linearized 1 /L {length} /H [ 0 0 ] /O 46 /E 0 /N 1 /T 0 >>"),
        );
        b.object(44, "<< /Type /Catalog /Pages 45 0 R >>");
        b.object(45, "<< /Type /Pages /Kids [46 0 R] /Count 1 >>");
        b.object(46, "<< /Type /Page /Parent 45 0 R /MediaBox [0 0 10 10] >>");
        b.build(44)
    }

    /// The same file with `/L` patched to its actual length; the
    /// placeholder keeps the width, so the length does not move.
    fn current_linearized_file() -> Vec<u8> {
        let mut data = linearized_file("0000000000");
        let length = format!("{:010}", data.len());
        let at = data.windows(10).position(|w| w == b"0000000000").unwrap();
        data[at..at + 10].copy_from_slice(length.as_bytes());
        data
    }

    // Covers ISO 32000-1 Annex F.3.
    #[test]
    fn reads_every_entry_of_the_linearization_parameter_dictionary() {
        let record = linearization_dictionary(HEAD).unwrap();
        assert_eq!(
            record,
            Linearization {
                version: 1.0,
                file_length: 12345,
                hint_streams: vec![(500, 200)],
                first_page_object: 45,
                first_page_end: 3000,
                page_count: 3,
                main_xref_offset: 11000,
                first_page: 2,
            }
        );
    }

    // Covers ISO 32000-1 Annex F.3.
    #[test]
    fn the_first_page_number_defaults_to_zero_and_an_overflow_hint_stream_is_read() {
        let head = b"%PDF-1.5\n7 0 obj\n<< /Linearized 1.0 /L 99 /H [ 500 200 8000 100 ] /O 9 /E 30 /N 1 /T 90 >>\nendobj\n";
        let record = linearization_dictionary(head).unwrap();
        assert_eq!(record.hint_streams, vec![(500, 200), (8000, 100)]);
        assert_eq!(record.first_page, 0);
        assert_eq!(record.version, 1.0);
    }

    /// Only the first object counts, it must sit within the first 1024
    /// bytes, and every required entry of Table F.1 must be present.
    // Covers ISO 32000-1 Annex F.3.
    #[test]
    fn a_file_without_the_dictionary_as_its_first_object_is_not_linearized() {
        assert_eq!(linearization_dictionary(b""), None);
        assert_eq!(
            linearization_dictionary(b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n"),
            None
        );
        let missing_page_count =
            b"%PDF-1.4\n43 0 obj\n<< /Linearized 1 /L 12345 /H [ 500 200 ] /O 45 /E 3000 /T 11000 >>\nendobj\n";
        assert_eq!(linearization_dictionary(missing_page_count), None);
        let odd_hint_array =
            b"%PDF-1.4\n43 0 obj\n<< /Linearized 1 /L 12345 /H [ 500 ] /O 45 /E 3000 /N 3 /T 11000 >>\nendobj\n";
        assert_eq!(linearization_dictionary(odd_hint_array), None);
        let mut late = b"%PDF-1.4\n%".to_vec();
        late.extend(std::iter::repeat_n(b'x', 1024));
        late.extend_from_slice(&HEAD[9..]);
        assert_eq!(linearization_dictionary(&late), None);
    }

    /// `/L` must equal the file's length for the linearization information
    /// to apply; after an appended update the dictionary is still readable
    /// but the file counts as ordinary PDF.
    // Covers ISO 32000-1 Annex F.3.
    #[test]
    fn a_document_is_linearized_only_while_the_declared_length_matches() {
        let doc = Document::load(current_linearized_file()).unwrap();
        let record = doc.linearization().unwrap();
        assert_eq!(record.first_page_object, 46);
        assert_eq!(record.page_count, 1);
        assert!(doc.is_linearized());

        let mut updated = current_linearized_file();
        updated.extend_from_slice(b"%appended update\n");
        let doc = Document::load(updated).unwrap();
        assert!(doc.linearization().is_some());
        assert!(!doc.is_linearized());

        let doc = Document::load(pdfboss_testkit::simple_doc("plain")).unwrap();
        assert_eq!(doc.linearization(), None);
        assert!(!doc.is_linearized());
    }
}
