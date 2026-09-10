//! Page-piece dictionaries (ISO 32000-1 §14.5): the private product data a
//! `/PieceInfo` entry holds on the catalog, a page or a form XObject, read
//! as data and left uninterpreted.

use crate::date::Date;
use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// One product's data dictionary (ISO 32000-1 §14.5, Table 319) from a
/// page-piece dictionary (Table 318): the private data the named conforming
/// product left on the document, a page or a form.
#[derive(Debug, Clone, PartialEq)]
pub struct PagePiece {
    /// The page-piece dictionary's key: the product's name, or a well-known
    /// data type a family of products shares.
    pub product: String,
    /// `/LastModified` as written: when the product last altered the
    /// content. The clause compares it with the page's or form's own
    /// `/LastModified` for equality only, never for order.
    pub last_modified: Option<String>,
    /// `/Private`: the product's data as written, typically a dictionary.
    pub private: Option<Object>,
}

impl PagePiece {
    /// `/LastModified` as a date (§7.9.4); `None` when absent or not a date
    /// the parser accepts.
    ///
    /// Covers ISO 32000-1 §14.5.
    pub fn last_modified_parsed(&self) -> Option<Date> {
        Date::parse_pdf(self.last_modified.as_deref()?)
    }
}

/// The page-piece dictionary `dict`'s `/PieceInfo` holds, one record per
/// product sorted by name since a dictionary keeps no order: every entry
/// whose value is a dictionary, the required `/LastModified` tolerated
/// missing. `dict` may be the catalog (Table 28), a page (Table 30) or a
/// form XObject (Table 95); no `/PieceInfo`, or one that is no dictionary,
/// yields none.
///
/// Covers ISO 32000-1 §14.5.
pub async fn piece_info_with<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Vec<PagePiece> {
    let mut pieces = Vec::new();
    let Some(entry) = dict.get("PieceInfo") else {
        return pieces;
    };
    let Some(piece_info) = resolved_dict(src, entry).await else {
        return pieces;
    };
    for (product, value) in piece_info.iter() {
        let Some(data) = resolved_dict(src, value).await else {
            continue;
        };
        let entries = Entries { src, dict: &data };
        pieces.push(PagePiece {
            product: product.0.clone(),
            last_modified: entries.text("LastModified").await,
            private: entries.value("Private").await,
        });
    }
    pieces.sort_by(|a, b| a.product.cmp(&b.product));
    pieces
}

/// The catalog's page-piece dictionary (ISO 32000-1 §14.5, Table 28) read
/// through the trailer's `/Root`; empty without a catalog or a `/PieceInfo`.
///
/// Covers ISO 32000-1 §14.5.
pub async fn document_piece_info_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Vec<PagePiece> {
    let Some(root) = trailer.get("Root") else {
        return Vec::new();
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return Vec::new();
    };
    piece_info_with(src, &catalog).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::object::{Dict, Name};
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document: `catalog_extra` goes into the catalog,
    /// `page_extra` into the page, and object 7 is a product's data
    /// dictionary reachable by reference.
    fn doc_with(catalog_extra: &str, page_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page_extra} >>"),
        );
        b.object(
            7,
            "<< /LastModified (D:20230601120000Z) /Private (opaque) >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// Each product's data dictionary is read, sorted by product name; the
    /// date is kept as written and parses on request; a non-dictionary
    /// entry is skipped.
    // Covers ISO 32000-1 §14.5.
    #[test]
    fn reads_each_products_data_dictionary_of_a_page_piece_dictionary() {
        let doc = doc_with(
            "/PieceInfo << /Photoshop 7 0 R /Junk 5 \
             /Illustrator << /LastModified (D:20240102030405Z) /Private << /Version 28 >> >> >>",
            "",
        );
        let pieces = doc.piece_info();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].product, "Illustrator");
        assert_eq!(
            pieces[0].last_modified.as_deref(),
            Some("D:20240102030405Z")
        );
        assert_eq!(
            pieces[0].last_modified_parsed().map(|d| d.to_iso8601()),
            Some("2024-01-02T03:04:05Z".to_string())
        );
        let mut version = Dict::new();
        version.insert(Name("Version".to_string()), Object::Int(28));
        assert_eq!(pieces[0].private, Some(Object::Dict(version)));
        assert_eq!(pieces[1].product, "Photoshop");
        assert_eq!(
            pieces[1].last_modified.as_deref(),
            Some("D:20230601120000Z")
        );
        assert_eq!(pieces[1].private, Some(Object::String(b"opaque".to_vec())));
    }

    /// A page's `/PieceInfo` reads the same way; a catalog or page without
    /// one, or whose entry is no dictionary, has no pieces; a data
    /// dictionary without `/LastModified` still counts, the date being the
    /// only required entry and this reader being lenient.
    // Covers ISO 32000-1 §14.5.
    #[test]
    fn a_pages_piece_info_reads_the_same_way_and_a_missing_entry_is_empty() {
        let doc = doc_with(
            "/PieceInfo 42",
            "/PieceInfo << /Scanner << /Private << /Dpi 300 >> >> >>",
        );
        assert_eq!(doc.piece_info(), vec![]);
        let page = doc.page(0).unwrap();
        let pieces = doc.page_piece_info(&page);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].product, "Scanner");
        assert_eq!(pieces[0].last_modified, None);
        assert_eq!(pieces[0].last_modified_parsed(), None);
        let plain = doc_with("", "");
        assert_eq!(plain.piece_info(), vec![]);
        assert_eq!(plain.page_piece_info(&plain.page(0).unwrap()), vec![]);
    }
}
